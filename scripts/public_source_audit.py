#!/usr/bin/env python3
"""Audit an explicit Git source candidate without publishing or changing it."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def path_issue(name, mode="100644"):
    parts = PurePosixPath(name).parts
    if not parts or name.startswith("/") or ".." in parts:
        return "Unsafe source path"
    if mode not in ("100644", "100755"):
        return "Symlink, submodule or unsupported source mode requires review"
    if any(part in {".local", ".aws", ".codex", "node_modules", "target", "__pycache__"} for part in parts):
        return "Private data, dependency cache or build output"
    leaf = parts[-1].lower()
    if leaf.startswith(".env") and leaf != ".env.example":
        return "Environment configuration"
    if leaf.endswith((".dump", ".pgdump", ".sql.gz", ".sqlite", ".sqlite3", ".db", ".p12", ".pfx", ".key")):
        return "Database or credential container requires review"
    return None


def classify(finding, path, data, reviews):
    identity = {"rule": finding["RuleID"], "path": path,
                "start_line": finding["StartLine"], "end_line": finding["EndLine"],
                "source_sha256": sha256(data)}
    matches = [review for review in reviews if all(review.get(key) == value for key, value in identity.items())]
    accepted = len(matches) == 1 and bool(matches[0].get("reason", "").strip())
    return {**identity, "commit": finding.get("Commit", ""),
            "classification": "reviewed_false_positive" if accepted else "requires_review",
            "reason": matches[0]["reason"] if accepted else "No exact source/rule/line review"}


def audit(repository, scanner, output, review_path):
    repository = repository.resolve()
    scanner = scanner.resolve()
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    raw = output / "raw"
    raw.mkdir(mode=0o700)
    env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
    for name in ("GITLEAKS_CONFIG", "GITLEAKS_CONFIG_TOML"):
        env.pop(name, None)

    def git(*args):
        return subprocess.run(["git", *args], cwd=str(repository), env=env,
                              check=True, capture_output=True).stdout

    if git("rev-parse", "--is-shallow-repository").strip() != b"false":
        raise ValueError("Full history is required; shallow candidates cannot pass")
    refs = [dict(zip(("ref", "object"), line.split(" ", 1)))
            for line in git("for-each-ref", "--format=%(refname) %(objectname)").decode().splitlines()]
    if not refs:
        raise ValueError("No source refs to audit")
    head = git("rev-parse", "HEAD").decode().strip()
    commits = git("rev-list", "--all").decode().splitlines()
    tree = raw / "tree"
    tree.mkdir(mode=0o700)
    issues = []
    current = {}
    history_paths = set()
    history_blobs = set()
    binary_files = []
    for commit in commits:
        for entry in git("ls-tree", "-r", "-z", commit).split(b"\0"):
            if not entry:
                continue
            info, name = entry.split(b"\t", 1)
            mode, kind, blob = info.decode().split()
            name = name.decode("utf8")
            history_paths.add(name)
            if kind == "blob":
                history_blobs.add(blob)
            issue = path_issue(name, mode)
            if issue:
                issues.append({"commit": commit, "path": name, "mode": mode, "reason": issue})
            if commit == head:
                if issue:
                    continue
                data = git("cat-file", "blob", blob)
                path = tree / name
                path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                path.write_bytes(data)
                path.chmod(0o700 if mode == "100755" else 0o600)
                current[name] = {"git_blob": blob, "mode": mode, "sha256": sha256(data), "bytes": len(data)}
                if b"\0" in data:
                    binary_files.append(name)
    if not current:
        raise ValueError("HEAD is not reachable from audited refs or has no ordinary files")
    metadata = raw / "metadata"
    metadata.mkdir(mode=0o700)
    (metadata / "commits.txt").write_bytes(git("log", "--all", "--format=fuller", "--no-patch"))
    (raw / "source-files.json").write_text(json.dumps(current, indent=2) + "\n")
    config = raw / "scanner.toml"
    config.write_text("[extend]\nuseDefault = true\n")
    ignore = raw / "empty-ignore"
    ignore.write_text("")
    version = subprocess.run([str(scanner), "version"], check=True, capture_output=True, text=True).stdout.strip()
    reviews_raw = review_path.read_bytes()
    reviews = json.loads(reviews_raw)["reviews"]
    scans = []
    findings = []
    commands = [("history", ["git", str(repository), "--log-opts=--all --full-history"]),
                ("current_tree", ["dir", str(tree)]), ("commit_metadata", ["dir", str(metadata)])]
    for name, args in commands:
        report = raw / (name + "-findings.json")
        command = [str(scanner), *args, "--config", str(config), "--gitleaks-ignore-path", str(ignore),
                   "--ignore-gitleaks-allow", "--redact=100", "--max-target-megabytes=0",
                   "--max-archive-depth=3", "--max-decode-depth=5", "--no-banner", "--no-color",
                   "--report-format=json", "--report-path", str(report)]
        with (raw / (name + ".log")).open("wb") as log:
            result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        if result.returncode not in (0, 1) or not report.is_file():
            raise RuntimeError("Scanner failed; retained diagnostics: " + name)
        candidates = json.loads(report.read_text())
        if bool(candidates) != (result.returncode == 1):
            raise RuntimeError("Scanner report/exit status disagree: " + name)
        scans.append({"name": name, "exit_code": result.returncode, "candidates": len(candidates)})
        for item in candidates:
            if name == "history":
                path = item["File"]
                commit = item.get("Commit", "")
                if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
                    raise ValueError("Invalid original finding commit")
                data = git("show", commit + ":" + path)
            elif name == "current_tree":
                found_path = Path(item["File"]).resolve()
                path = str(found_path.relative_to(tree.resolve()))
                data = found_path.read_bytes()
            else:
                path = "commit-metadata/" + Path(item["File"]).name
                data = (metadata / Path(item["File"]).name).read_bytes()
            findings.append({"scan": name, **classify(item, path, data, reviews)})
    unreviewed = sum(item["classification"] == "requires_review" for item in findings)
    report = {"schema_version": 1, "recorded_at": datetime.now(timezone.utc).isoformat(),
              "scope": "Automated source/history checks with exact-byte false-positive review; no publication",
              "result": "passed" if not issues and not unreviewed else "held",
              "head": head, "refs": refs, "commits": len(commits), "current_files": len(current),
              "historical_paths": len(history_paths), "unique_historical_blobs": len(history_blobs),
              "source_files_manifest_sha256": sha256((raw / "source-files.json").read_bytes()),
              "scanner": {"version": version, "binary_sha256": sha256(scanner.read_bytes()),
                          "default_rules": True, "inline_allow_comments_ignored": True,
                          "repo_ignore_file_ignored": True, "redaction_percent": 100,
                          "review_sha256": sha256(reviews_raw)},
              "scans": scans, "findings": findings, "unreviewed_findings": unreviewed,
              "path_issues": issues, "binary_files": binary_files,
              "public_launch_authorized": False, "production_qualified": False,
              "limits": ["Pattern scanning does not prove absence of secrets or customer data",
                         "Binary assets require provenance review; Git patch scanning follows scanner defaults",
                         "Only refs available to this clone are included; server-side deleted/unreachable objects are outside scope",
                         "GitHub issues, pull requests, Actions logs/artifacts and releases require separate review",
                         "Rights to publish and security-contact ownership require maintainer confirmation",
                         "A passing source audit does not replace exact-source software release qualification"]}
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True, help="Explicit full Git checkout or mirror; never modified")
    parser.add_argument("--gitleaks", type=Path, required=True, help="Locally verified Gitleaks executable")
    parser.add_argument("--output", type=Path, required=True, help="New private evidence directory; refuses overwrite")
    parser.add_argument("--reviews", type=Path, default=Path(__file__).with_name("public_source_reviews.json"))
    args = parser.parse_args()
    os.umask(0o077)
    report = audit(args.repository, args.gitleaks, args.output.resolve(), args.reviews.resolve())
    print(json.dumps({key: report[key] for key in ("result", "head", "commits", "current_files", "unreviewed_findings")}))
    raise SystemExit(0 if report["result"] == "passed" else 1)


if __name__ == "__main__":
    main()

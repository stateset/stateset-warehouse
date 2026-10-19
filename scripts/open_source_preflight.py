#!/usr/bin/env python3
"""Inspect a local public-source candidate; never publish or change visibility."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def inspect(root):
    required = ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "SECURITY.md", "CONTRIBUTING.md", "THIRD_PARTY_NOTICES.md", "docs/open-source-release.md", "docs/icommerce.md", "cli/README.md"]
    files = {}
    for name in required:
        raw = (root / name).read_bytes()
        if not raw.strip():
            raise ValueError("Empty release document: " + name)
        files[name] = hashlib.sha256(raw).hexdigest()
    upstream_root = root / "vendor/stateset-icommerce"
    upstream = json.loads((upstream_root / "UPSTREAM.json").read_text())
    expected = {row["path"] for row in upstream["files"]} | {"UPSTREAM.json", "Cargo.toml"}
    actual = {str(path.relative_to(upstream_root)) for path in upstream_root.rglob("*") if path.is_file()}
    if expected != actual:
        raise ValueError("Vendored dependency inventory differs from recorded upstream")
    for row in upstream["files"]:
        if hashlib.sha256((upstream_root / row["path"]).read_bytes()).hexdigest() != row["sha256"]:
            raise ValueError("Vendored source differs from reviewed upstream: " + row["path"])
    if hashlib.sha256((upstream_root / "Cargo.toml").read_bytes()).hexdigest() != upstream["consumer_workspace_sha256"]:
        raise ValueError("Consumer workspace differs from recorded packaging")
    for name in ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE"]:
        if (root / name).read_bytes() != (upstream_root / name).read_bytes():
            raise ValueError("Root license choice differs from retained StateSet license: " + name)
    # Inventory the actual host target offline, including development crates.
    # Other targets remain an explicit public-distribution review item.
    target_info = subprocess.run(["rustc", "-vV"], check=True, capture_output=True, text=True).stdout
    target = next(line.split(": ", 1)[1] for line in target_info.splitlines() if line.startswith("host: "))
    result = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--filter-platform", target, "--format-version", "1"], cwd=str(root), check=True, capture_output=True, text=True)
    metadata = json.loads(result.stdout)
    rust = [{"name": p["name"], "version": p["version"], "license": p.get("license"), "license_file": Path(p["license_file"]).name if p.get("license_file") else None, "source": p.get("source") or "repository path"} for p in metadata["packages"]]
    package = next(p for p in metadata["packages"] if p["name"] == "stateset-warehouse")
    for name in ["dashboard/package.json", "cli/package.json"]:
        consumer = json.loads((root / name).read_text())
        if consumer["version"] != package["version"] or consumer.get("license") != "MIT OR Apache-2.0":
            raise ValueError("Version/license mismatch: " + name)
    missing = [p["name"] + "@" + p["version"] for p in rust if not p["license"] and not p["license_file"]]
    if missing:
        raise ValueError("Review missing Rust license declarations: " + ", ".join(missing))
    lock = json.loads((root / "dashboard/package-lock.json").read_text())
    browser = []
    for name, p in lock["packages"].items():
        if not name:
            continue
        manifest_path = root / "dashboard" / name / "package.json"
        installed = json.loads(manifest_path.read_text()) if manifest_path.is_file() else {}
        browser.append({"package": name, "version": p["version"], "license": p.get("license") or installed.get("license"), "optional": p.get("optional", False), "installed_metadata_available": bool(installed)})
    return {"schema_version": 1, "scope": "Local source preparation and declared dependency-license inventory; no visibility change, package publication or legal approval", "documents_sha256": files, "icommerce_revision": upstream["revision"], "vendored_files_verified": len(upstream["files"]), "rust_target": target, "rust_packages": sorted(rust, key=lambda p: (p["name"], p["version"])), "browser_packages": browser, "terminal_runtime_dependencies": [], "remaining_review": ["Full public tree and Git history secret/customer-data review", "License texts and compatibility for the actual resolved/distributed artifacts", "Other Rust targets and optional browser declarations unavailable on this platform", "Container image/base-package distribution notices", "Exact-source release qualification and deployed/physical acceptance"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, help="Write a new review artifact; refuses overwrite")
    args = parser.parse_args()
    result = inspect(Path(__file__).resolve().parent.parent)
    if args.output:
        with args.output.open("x") as stream:
            json.dump(result, stream, indent=2)
            stream.write("\n")
        print("Verified %d upstream files; inventoried %d Rust and %d browser packages. Review: %s" % (result["vendored_files_verified"], len(result["rust_packages"]), len(result["browser_packages"]), args.output))
    else:
        json.dump(result, sys.stdout, indent=2)
        print()


if __name__ == "__main__":
    main()

"""Private, durable, immutable checkpoints outside the measured traffic interval."""

import hashlib
import json
import os
from pathlib import Path
import re
import tempfile


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


class CrashJournal:
    def __init__(self, root, phase, run, actor, request):
        if phase not in ("api_sigkill", "postgres_sigkill"):
            raise ValueError("Unexpected crash phase")
        parent = Path(root) / "crash-journal"
        try:
            parent.mkdir(mode=0o700)
            sync_directory(parent.parent)
        except FileExistsError:
            if parent.is_symlink() or not parent.is_dir():
                raise ValueError("Unsafe crash journal directory")
        self.directory = parent / phase
        self.directory.mkdir(mode=0o700)  # Never overwrite a previous attempt.
        sync_directory(parent)
        self.entries = []
        # Freeze the exact HTTP serialization before any request or fault is sent.
        wire = json.dumps(request, separators=(",", ":"))
        self.record("prepared", run=run, actor=actor, original_request=json.loads(wire),
                    http_body=wire, http_body_sha256=hashlib.sha256(wire.encode()).hexdigest())

    def record(self, stage, **facts):
        if not re.fullmatch(r"[a-z][a-z_]*", stage):
            raise ValueError("Unsafe checkpoint name")
        name = f"{len(self.entries):03}-{stage}.json"
        raw = (json.dumps({"stage": stage, "facts": facts}, sort_keys=True, indent=2) + "\n").encode()
        descriptor, temporary = tempfile.mkstemp(prefix=".pending-", dir=self.directory)
        try:
            with os.fdopen(descriptor, "wb") as output:
                output.write(raw)
                output.flush()
                os.fsync(output.fileno())
            # Publish only a complete, synced file; hard-link creation refuses replacement.
            os.link(temporary, self.directory / name)
            sync_directory(self.directory)
        finally:
            os.unlink(temporary)
        self.entries.append({"path": f"crash-journal/{self.directory.name}/{name}",
                             "sha256": hashlib.sha256(raw).hexdigest()})

    def manifest(self):
        for entry in self.entries:
            path = self.directory / Path(entry["path"]).name
            if hashlib.sha256(path.read_bytes()).hexdigest() != entry["sha256"]:
                raise AssertionError("Crash journal checkpoint changed")
        return {"private_checkpoints": [entry.copy() for entry in self.entries],
                "contains_credentials": False}


def verify_journal(root, manifest, phase):
    """Check retained bytes and checkpoint order independently of the live writer."""
    records = []
    for index, entry in enumerate(manifest["private_checkpoints"]):
        prefix = f"crash-journal/{phase}/{index:03}-"
        name = entry["path"]
        if not name.startswith(prefix) or not re.fullmatch(r"[a-z][a-z_]*\.json", name[len(prefix):]):
            raise AssertionError("Unexpected crash journal checkpoint path")
        raw = (Path(root) / name).read_bytes()
        if hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            raise AssertionError("Crash journal checkpoint digest mismatch")
        record = json.loads(raw)
        if name != prefix + record["stage"] + ".json":
            raise AssertionError("Crash journal stage differs from filename")
        records.append(record)
    stages = [record["stage"] for record in records]
    required = ["prepared", "baseline_captured", "original_send_requested", "writer_blocked",
                "fault_requested", "interrupted_response_observed", "fault_verified",
                "original_absent", "original_recovery_requested", "original_committed",
                "exact_replay_verified", "completed"]
    positions = [stages.index(stage) for stage in required]
    if stages[0] != "prepared" or stages[-1] != "completed" or positions != sorted(positions) or len(set(stages)) != len(stages):
        raise AssertionError("Incomplete or reordered crash journal")
    original = records[0]["facts"]
    wire = original["http_body"]
    if (json.loads(wire) != original["original_request"]
            or hashlib.sha256(wire.encode()).hexdigest() != original["http_body_sha256"]):
        raise AssertionError("Crash journal original HTTP body mismatch")
    return records

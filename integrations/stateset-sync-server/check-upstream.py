#!/usr/bin/env python3
"""Typecheck receiver/router against an existing checked upstream crate cache.

Reads the upstream cache and writes only a temporary local artifact. This does
not rebuild the entire OMS or exercise its authentication middleware. --tests
also executes receiver input/digest tests against cached real dependencies.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("upstream", type=Path)
    parser.add_argument("--tests", action="store_true")
    parser.add_argument("--postgres", action="store_true", help="also run the ignored real SQLx/PostgreSQL proof")
    args = parser.parse_args()
    args.tests = args.tests or args.postgres
    fingerprint_dir = args.upstream / "target/debug/.fingerprint"
    dependencies = args.upstream / "target/debug/deps"
    candidates = []
    extension = "rlib" if args.tests else "rmeta"
    for manifest in fingerprint_dir.glob("stateset-sync-server-*/lib-stateset_sync_server.json"):
        name = manifest.parent.name.replace("stateset-sync-server-", "stateset_sync_server-")
        artifact = dependencies / f"lib{name}.{extension}"
        if artifact.is_file() and artifact.stat().st_size:
            candidates.append((artifact.stat().st_mtime, manifest, artifact))
    if not candidates:
        raise SystemExit("No checked upstream library cache; run the normal upstream cargo check/build first")
    _, manifest, artifact = max(candidates)
    command = [
        "rustc", "--edition", "2021", "--crate-name", "warehouse_receiver_compat",
        "-L", f"dependency={dependencies}", "--extern", f"stateset_sync_server={artifact}",
    ]
    if args.tests:
        command += ["--test"]
    else:
        command += ["--crate-type", "lib", "--emit=metadata"]
    required = {"axum", "chrono", "serde", "serde_json", "sha2", "uuid", "sqlx", "tokio"}
    resolved = set()
    for _, name, _, digest in json.loads(manifest.read_text())["deps"]:
        if name not in required:
            continue
        matches = []
        for candidate in fingerprint_dir.glob(f"{name}-*/lib-{name}"):
            try:
                matches_digest = int.from_bytes(bytes.fromhex(candidate.read_text().strip()), "little") == digest
            except ValueError:
                continue
            path = dependencies / f"lib{candidate.parent.name}.{extension}"
            if matches_digest and path.is_file():
                matches.append(path)
        if len(matches) != 1:
            raise SystemExit(f"Cannot resolve exact cached {name} dependency; run the normal upstream build")
        command += ["--extern", f"{name}={matches[0]}"]
        resolved.add(name)
    if resolved != required:
        raise SystemExit(f"Incomplete cached dependency set: {required-resolved}")
    source = (
        "pub use stateset_sync_server::http;\n"
        f"#[path={json.dumps(str(ROOT/'src/warehouse_fulfillment.rs'))}] pub mod warehouse_fulfillment;\n"
        "pub fn mount()->axum::Router<http::state::AppState>{axum::Router::new().route("
        '\"/v1/tenants/:tenant_id/warehouse-fulfillments\",'
        "axum::routing::get(warehouse_fulfillment::capabilities).post(warehouse_fulfillment::receive))}\n"
    )
    with tempfile.TemporaryDirectory(prefix="stateset-oms-compat-") as temporary:
        temporary = Path(temporary)
        wrapper = temporary / "compat.rs"
        wrapper.write_text(source)
        output = temporary / ("receiver-tests" if args.tests else "receiver.rmeta")
        subprocess.run(command + [str(wrapper), "-o", str(output)], check=True, timeout=180)
        if args.tests:
            subprocess.run([str(output)] + (["--include-ignored"] if args.postgres else []), check=True, timeout=60)
    print("PASS: receiver and Axum router use actual upstream AppState, TenantRequest, ApiError, ApiSuccess and database pool types (cached library; full upstream rebuild/auth middleware not exercised)")


if __name__ == "__main__":
    main()

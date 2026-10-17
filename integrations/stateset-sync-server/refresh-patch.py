#!/usr/bin/env python3
"""Refresh the reviewable companion patch without changing the upstream tree."""
import argparse
import difflib
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent
CHANGES = {
    "src/lib.rs": ("mod webhooks;", "pub mod warehouse_fulfillment;\nmod webhooks;"),
    "src/http/router.rs": (
        '        .route(\n            "/v1/tenants/:tenant_id/orders/:order_id/fulfillment",',
        '        .route(\n            "/v1/tenants/:tenant_id/warehouse-fulfillments",\n'
        '            get(crate::warehouse_fulfillment::capabilities)\n'
        '                .post(crate::warehouse_fulfillment::receive),\n'
        '        )\n        .route(\n            "/v1/tenants/:tenant_id/orders/:order_id/fulfillment",',
    ),
    "src/http/rbac.rs": (
        "    // Order endpoints\n    if path.contains",
        '    // Complete-order WMS handoff evidence uses the existing order scopes.\n'
        '    if path.ends_with("/warehouse-fulfillments") {\n'
        '        return if matches!(method.as_str(), "GET" | "HEAD") {\n'
        '            RequiredScopes::single(Scope::OrdersRead)\n'
        '        } else {\n            RequiredScopes::single(Scope::OrdersWrite)\n'
        '        };\n    }\n\n    // Order endpoints\n    if path.contains',
    ),
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("upstream", type=Path)
    args = parser.parse_args()
    patch = []
    for name, (old, new) in CHANGES.items():
        before = (args.upstream / name).read_text()
        if before.count(old) != 1:
            raise SystemExit(f"Upstream context changed: {name}; review manually")
        after = before.replace(old, new)
        patch.append(f"diff --git a/{name} b/{name}\n")
        patch.extend(difflib.unified_diff(before.splitlines(True), after.splitlines(True), fromfile=f"a/{name}", tofile=f"b/{name}"))
    for name in ["src/warehouse_fulfillment.rs", "migrations/20261017000000_warehouse_fulfillment_receipts.sql"]:
        patch.append(f"diff --git a/{name} b/{name}\nnew file mode 100644\n")
        patch.extend(difflib.unified_diff([], (ROOT/name).read_text().splitlines(True), fromfile="/dev/null", tofile=f"b/{name}"))
    path = ROOT / "upstream.patch"
    path.write_text("".join(patch))
    subprocess.run(["git", "-C", str(args.upstream), "apply", "--check", str(path)], check=True)
    print("PASS: refreshed companion patch applies cleanly; upstream worktree unchanged")


if __name__ == "__main__":
    main()

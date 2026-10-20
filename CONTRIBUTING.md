# Contributing

Contributions are welcome under **MIT OR Apache-2.0**, at the recipient's option. By submitting a contribution, you represent that you have the right to license it under those terms. Retain third-party attribution and do not introduce incompatible source or generated assets.

Start with the [architecture](docs/architecture.md), [API contract](docs/api.md) and [verification instructions](README.md#verify). Use Rust 1.90+, Node.js 22.12+ and an isolated PostgreSQL database whose name contains `test`. Never use real inventory or external production integrations for tests.

```bash
npm --prefix dashboard ci
bash scripts/check.sh
# Requires installed PostgreSQL server binaries; creates an isolated cluster.
bash scripts/local-postgres.sh
```

Use `cargo fmt --package stateset-warehouse`, because `cargo fmt --all` also reformats vendored dependencies. Vendored iCommerce sources must match their recorded public upstream revision. Run `python3 -m unittest discover -s scripts -p 'test_icommerce_upstream.py'` after dependency changes. Read [iCommerce interoperability](docs/icommerce.md) before changing mappings or quantity rules.

Keep inventory writes in the canonical command transaction. Preserve authenticated tenant/actor scope, original request identity, physical observations, immutable receipts, version fences, movement/reservation equations and append-only history. Add meaningful tests for changed invariants and failure paths. A UI or MCP adapter must not create an independent inventory writer.

Describe the concrete behavior change, relevant checks and remaining limits in a pull request. Use synthetic examples. Do not commit secrets, dumps, private qualification artifacts, build output or dependency caches. Follow [security reporting](SECURITY.md) for vulnerabilities.

After changing browser dependencies, regenerate their original notices with `python3 scripts/dashboard_notices.py` after `npm --prefix dashboard ci`. Ordinary checks reject missing or stale notices. Full-history source scanning uses `scripts/public_source_audit.py` as described in the [public-release checklist](docs/open-source-release.md). A known false positive is bound to exact source bytes, path, rule and line range; never suppress a live credential or ignore a whole fixture directory.

Release qualification is separate from ordinary checks. Preserve failed attempts and publish results only for the exact source and image tested. A passing local release does not imply deployed-client, facility or production acceptance. Repository visibility and package publication are explicit release decisions; adding license files does not publish this repository.

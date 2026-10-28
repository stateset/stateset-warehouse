# Open-source release preparation

The source is prepared for **MIT OR Apache-2.0**, matching the included iCommerce crates. Root license texts, [security reporting](../SECURITY.md), [contribution instructions](../CONTRIBUTING.md), [third-party attribution](../THIRD_PARTY_NOTICES.md), portable pinned crate sources and terminal documentation are included. The repository remains private; these files do not change visibility or publish a package/image.

Run `python3 scripts/open_source_preflight.py --output /tmp/wms-source-license-review.json` to verify the vendored snapshot and inventory declared dependency licenses for the host Rust target and browser lockfile. It refuses to overwrite a review artifact and records remaining distribution checks; it does not grant legal approval. Install dashboard dependencies first to include available browser package metadata.

For a reproducible source/history scan, use an explicit **full** Git checkout or mirror and a locally verified [Gitleaks](https://github.com/gitleaks/gitleaks) executable:

```bash
python3 scripts/public_source_audit.py \
  --repository /path/to/full-checkout \
  --gitleaks /path/to/verified/gitleaks \
  --output /tmp/wms-new-private-source-review
```

The output directory must not exist. Keep it private: `raw/` includes an exact source export, original commit metadata and redacted scanner diagnostics. Only `report.json` is designed for publication after review. The auditor checks all available refs and historical paths, scans the full patch history, exact HEAD files and commit metadata, and rejects shallow history. It ignores repository scanner configurations, ignore files and inline allow comments. Known false positives require an exact path, full-file SHA256, rule, line range and explanation in `scripts/public_source_reviews.json`; changing any of those requires a new review. Unknown findings and private data paths hold the result. Pattern scanning cannot establish that all material is free of secrets/customer data or that the maintainer holds publication rights. Binary assets and GitHub metadata need separate review.

The workflow obtains pinned Gitleaks 8.30.1 from its official release, verifies the archive SHA256, fetches full history and runs the same auditor. It uploads only the sanitized report, never the raw source/evidence directory. An unstarted hosted job is not a passing audit. Browser builds retain their original dependency texts at `/THIRD_PARTY_NOTICES.txt`; `python3 scripts/dashboard_notices.py --check` verifies the checked-in notice file after dependency installation.

The proposed public scope is the WMS core, canonical API, dashboard/scanner workflows, native MCP server, terminal client and integration contracts. Managed hosting, deployment operations, backups and support can be offered separately. Customer mappings, credentials, operational databases, private evidence and infrastructure access are excluded from public artifacts.

A public launch requires a concrete review of the exact source and artifacts:

1. Run ordinary checks and full release qualification on frozen source. Preserve failures and the prior accepted report. The source44 full gate passes final serial reconciliation at the unchanged SQL timeout; retain the earlier failed attempt and its limits.
2. Review the complete public tree and history for credentials and customer data. Keep `.env`, `.local`, dumps, tokens, local journals and dependency caches private. Synthetic public evidence must be sanitized and independently hashed.
3. Verify vendored sources against the public commit and review resolved dependency licenses, notices, container base packages and any distributed assets for the actual artifact.
4. Verify documentation links, fresh-checkout build instructions, private vulnerability reporting and release notes. Record precise software test scope and unresolved deployed/physical acceptance separately.
5. Review source/package/image version and immutable tag targets. Publish only artifacts whose checks and source/image fingerprints match the release record.
6. Change repository visibility or publish packages/images only with an explicit launch instruction after the reviewable result is ready.

The [0.4.1 qualification](qualification/release-20261027-v0.4.1/verification.json) binds tag `v0.4.1` to its complete full-profile gate, byte for byte. That record also includes the full-history source audit (0 unreviewed findings) and the dependency license review. It is local synthetic software evidence; deployed and physical acceptance remain separate. The dependency inventory records declared licenses rather than legal approval. Still outstanding before launch: hosted CI, publication-rights and security-inbox sign-off, container base-package notices for any published image, and an explicit launch instruction to change visibility.

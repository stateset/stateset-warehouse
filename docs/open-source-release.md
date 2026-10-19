# Open-source release preparation

The source is prepared for **MIT OR Apache-2.0**, matching the included iCommerce crates. Root license texts, [security reporting](../SECURITY.md), [contribution instructions](../CONTRIBUTING.md), [third-party attribution](../THIRD_PARTY_NOTICES.md), portable pinned crate sources and terminal documentation are included. The repository remains private; these files do not change visibility or publish a package/image.

Run `python3 scripts/open_source_preflight.py --output /tmp/wms-source-license-review.json` to verify the vendored snapshot and inventory declared dependency licenses for the host Rust target and browser lockfile. It refuses to overwrite a review artifact and records remaining distribution checks; it does not grant legal approval. Install dashboard dependencies first to include available browser package metadata.

The proposed public scope is the WMS core, canonical API, dashboard/scanner workflows, native MCP server, terminal client and integration contracts. Managed hosting, deployment operations, backups and support can be offered separately. Customer mappings, credentials, operational databases, private evidence and infrastructure access are excluded from public artifacts.

A public launch requires a concrete review of the exact source and artifacts:

1. Run ordinary checks and full release qualification on frozen source. Preserve failures and the prior accepted report. The source44 full gate passes final serial reconciliation at the unchanged SQL timeout; retain the earlier failed attempt and its limits.
2. Review the complete public tree and history for credentials and customer data. Keep `.env`, `.local`, dumps, tokens, local journals and dependency caches private. Synthetic public evidence must be sanitized and independently hashed.
3. Verify vendored sources against the public commit and review resolved dependency licenses, notices, container base packages and any distributed assets for the actual artifact.
4. Verify documentation links, fresh-checkout build instructions, private vulnerability reporting and release notes. Record precise software test scope and unresolved deployed/physical acceptance separately.
5. Review source/package/image version and immutable tag targets. Publish only artifacts whose checks and source/image fingerprints match the release record.
6. Change repository visibility or publish packages/images only with an explicit launch instruction after the reviewable result is ready.

The dated 0.2.0 qualification linked in the README binds the exact source, image and tested terminal package. It is local synthetic software evidence; deployed and physical acceptance remain separate. The dependency inventory records declared licenses rather than legal approval. Public launch, package/image publication and full history/distribution review remain outstanding.

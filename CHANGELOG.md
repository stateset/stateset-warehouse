# Changes

## Unreleased

- Prevent terminal recovery histories from forking when separate processes publish different outcomes at the same sequence position. Checkpoints claim one shared filename regardless of outcome, include a payload checksum, and report a concurrent change without overwriting the winner.
- Preserve exact original requests and existing checkpoints when continuing 0.2.0 journals. New journals use schema 2; close older terminal clients before upgrading. Previously forked histories remain held for review.

## 0.2.0 — 2026-10-19

- Compile pinned, portable StateSet iCommerce 1.39.0 core and primitive crates for strict typed product/move previews and read-only inventory projections. Explicit mappings retain original WMS identities. Exact decimal-string validation rejects fractions, overprecision, numeric JSON and unsupported fields before upstream decoding can round them. Canonical WMS commands remain the sole inventory writer.
- Add a Node.js warehouse TUI with typed operational forms, actual serial collection, facility selection and actor-bound durable original-request recovery. Include lost-success, real PostgreSQL/API and terminal acceptance tests.
- Add two read-only native MCP tools for iCommerce interoperability, bringing the administrator catalog to 70.
- Prepare dual MIT OR Apache-2.0 licensing, contribution/security instructions, third-party attribution, upstream source verification and dependency-license review. Repository visibility remains private.
- Preserve complete original crash requests and immutable checkpoints before fault injection and recovery in the release qualifier.
- Share complete original-source and physical serial reconciliation verdicts within one SQL statement without changing validity predicates, write guards or the ten-second timeout.

The fresh source44 complete gate passes: 331 Rust, 74 Python, 13 terminal and 170 browser checks, populated archive restore and both full image/crash profiles. The source42 full-reconciliation timeout and source43 intentional precision interruption remain retained failed attempts. SQL timeouts and migration digests remain unchanged. Deployed-client and physical facility acceptance remain separate. Repository visibility remains private; packages and images are not published.

## 0.1.1

Retained the serial after-write crash recovery qualification and original physical inventory proofs. See the immutable dated evidence linked from the README. Existing version tags are not moved to later development commits.

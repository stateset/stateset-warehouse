# Changes

## Unreleased

- Receive actual facility-transfer arrivals from selected original transfer rows and lines. Re-read the exact server position and check destination scope/version, role, source evidence and remaining in-transit work before collecting explicit quantity, dock, arrival reference and actual serial scans. Preserve the current server batch after row actions; source-side and settled transfer inspection cannot start receiving.
- Report exceptions from selected orders and investigate/resolve selected open exceptions from their table or queue. Preserve exact original order/exception IDs, require actual issue and resolution details, retain role/scope/version checks, and support linked order/shipment review. Declined confirmation creates no request; resolving an issue does not rewrite physical stock or fulfillment work.
- Keep original order/shipment reviews available when the server reports no returnable physical handoff. Display the original return-eligibility conflict as unavailable; authentication and service failures remain held.
- Replace first-page lot/transfer JSON with readable tables and complete server-batch navigation. Preserve exact cursors and original detail records, retain the displayed batch after failed reads, show server lot availability/check times, and include settled transfers with facility-direction filters. Search/filter/sort scope stays explicit per batch; these views remain read-only.
- Add work queues for order stages/urgency, incoming receipts, receiving-bin stock and open exceptions. Add F3/filter and F4/sort controls, retain filter/sort/search on refresh, and clear highlighted records on view changes before allowing an action. Preserve original identities and version checks; verify sorted order actions through the real API and terminal.
- Show serial capture progress and physical source context, retain earlier observations after duplicate/malformed scans, and support F2 correction before submission. Undo preserves original target groups and requires reconfirming zero observations. Sent request bytes and recovery checkpoints remain immutable.
- Open guided work from selected stock, order and receiving rows, including original record review and explicit original-line selection. Carry only exact source identities; require fresh role/scope/version checks, actual quantities, destinations and serial observations through the existing original-request journal.
- Add terminal palette colours, highlighted arrow/Tab navigation, full-screen buffer restoration, resize handling and a paged keyboard guide. Preserve literal scanner bursts and CRLF input; cancel forms/reviews with Esc before submission. Page complete original write reviews, outcomes and long inspection views, and explain invalid field entries.
- Add a responsive warehouse terminal overview, identity-wide pending recovery notice, menu search/shortcuts and searchable paged tables with complete record details. Preserve existing menu numbers, exact physical write reviews and headless JSON output; include 70-column real terminal acceptance.
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

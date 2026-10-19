# Warehouse terminal

The StateSet Warehouse TUI runs on Node.js 22.12+ on POSIX systems with no runtime npm dependencies. Local qualification uses Linux. It calls the canonical REST API and works with the same tenant, actor, roles, serial admission and immutable command receipts as the dashboard and MCP server.

```bash
export WMS_URL=https://warehouse.example.com
export WMS_TOKEN_FILE=/secure/path/operator-token
bash scripts/warehouse-tui.sh
# Read-only automation:
bash scripts/warehouse-tui.sh --warehouse wh-main --once inventory
```

Use an existing deployment with an individual operator credential. `WMS_TOKEN` is also supported; do not put bearer tokens in command arguments. Remote origins require HTTPS. Loopback HTTP is allowed for local development. Authentication redirects are refused. WorkOS users can supply a valid access token; this client does not implement browser sign-in or automatic token refresh. Update an expired credential and reopen the same recovery scope.

The numbered terminal menus provide facility selection, inventory/products/bins, receiving and returns, orders and shipment review, serial custody/history, lots, transfers, exceptions, retained history, reconciliation, operations and iCommerce interoperability. Typed forms cover physical workflows and administration. Inputs use whole each units. Multi-line work collects exact original line identities. Serial forms require actual literal scans; custody reads never prefill observed labels. A complete zero count needs explicit confirmation. Partial shipments, transfer dispatch and returns preserve their original target groups.

Forms capture a warehouse version before collecting work and retain it through submission. Creating a facility generates a new facility identity at version zero; a successful creation selects that facility. Concurrent facility work can cause a safe conflict; review again only after the previous original request is definitively resolved. Pack definitions remain readable through the API/dashboard; terminal forms currently enter whole each quantities directly rather than collecting pack scans. Replenishment planning and native OMS/TMS writeback remain available through the dashboard/API/MCP.

## Recovery

Before sending any physical command, the TUI writes its complete original JSON body and server/tenant/actor/facility scope into private recovery storage. It then fsyncs an immutable send-intent checkpoint. Files use 0600 and directories 0700, with exclusive publication, directory fsync and hash-linked history. Files contain operational request data, not bearer credentials. Default storage is `$XDG_STATE_HOME/stateset-warehouse` or `~/.local/state/stateset-warehouse`; set `WMS_TUI_STATE_DIR` or `--state-dir` for an explicit private directory. Storage must support local atomic hard links and fsync. Symlink recovery paths are rejected.

The development client uses schema 2 journals with one exclusive checkpoint filename per sequence position. Competing processes cannot publish different outcomes at the same position; the losing process must reopen the journal. Each checkpoint includes a payload checksum as well as the preceding file's hash, so an accidental change to the final outcome also holds recovery. These checks do not authenticate files against a malicious local owner.

Close older TUI processes before upgrading. The new client reads schema 1 journals from 0.2.0 and continues them with exclusive checkpoints, preserving all old files and the exact original HTTP body. Older clients cannot read the new format. Existing broken or forked histories remain held; the client never chooses an outcome or deletes evidence to repair them automatically.

A timeout, EOF, interrupted process, generic conflict, validation error or missing receipt retains the original request. Recovery shows the signed-in actor's work across facilities on the same server and tenant, including a newly created facility whose response was lost before it appeared in the directory. It can look up the actor-owned receipt at the original facility or resend the **exact original bytes**, with the original UUID, reviewed version, quantities, identities and serial observations. It never refreshes or replaces a pending request. A fully bound server proof can establish that a stale request never committed. Only completed work or that definitive rejection resolves a journal; unresolved work blocks new writes by that actor across facilities in the same recovery directory to prevent accidental duplicate physical recording.

Changing credentials to another actor, tenant or origin does not transfer pending work. Return to the original scope. Keep the recovery directory backed up privately; do not edit, delete, export publicly or fabricate receipts to bypass a hold. A disk error before a send checkpoint prevents transmission. A disk error after a response retains the original request for later receipt lookup/replay. The hash chain detects accidental modification; it is not a signature against a malicious local account owner.

`q` exits menus; Ctrl-C or EOF exits safely. Noninteractive use requires `--once`. Supported views are `session`, `warehouses`, `inventory`, `orders`, `receipts`, `reconciliation` and `recovery`. JSON output is read-only and may contain operational data. No headless write or automatic physical action is enabled.

## Check

```bash
npm --prefix cli test
```

The suite exercises lost-success replay, identity scope, disk failures before/after transmission, competing processes, legacy journal continuation, final-checkpoint tampering, stale proofs, redirect refusal, numeric precision, terminal control sanitization, typed forms and actual serial collection. API and database acceptance remain separate checks. See the root [verification instructions](https://github.com/stateset/stateset-warehouse#verify).

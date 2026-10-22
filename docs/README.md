# Documentation map

Pick the row that matches what you are doing.

## Trying it out

| Read | For |
|---|---|
| [Project README](../README.md#run) | Run the demo locally in a few commands. |
| [Warehouse terminal](../cli/README.md) | The keyboard/scanner client for warehouse floors. |
| [Visual console](visual-console.md) | Ghostty/Kitty graphics: flow, bin stock, floor plans, barcodes. |

## Integrating

| Read | For |
|---|---|
| [API contract](api.md) | Routes, commands, versions, retries and every inventory rule. |
| [Authentication](authentication.md) | API keys, roles and WorkOS sign-in. |
| [MCP server](mcp.md) | Tools for AI agents, with the same permissions and journal as REST. |
| [Native OMS/TMS adapters](native-integrations.md) | StateSet Sync orders and Freight planned loads. |
| [iCommerce interoperability](icommerce.md) | Typed previews and read-only projections with StateSet iCommerce. |

## Understanding and changing the code

| Read | For |
|---|---|
| [Architecture](architecture.md) | Ownership boundaries, inventory contract, persistence and concurrency. |
| [Serial tracking design](serial-design.md) | Plain-language guide to serial numbers, and how to change that code safely. |
| [Contributing](../CONTRIBUTING.md) | Toolchain, checks and the rules every change must keep. |
| [Performance](performance.md) | Workload harness and measured profiles. |

## Running it in production

| Read | For |
|---|---|
| [Operations and restore](operations.md) | Facilities, deployment, backup and restore into a fresh database. |
| [Observability](observability.md) | Operations snapshots, Prometheus metrics and alert policies. |
| [Resilience](resilience.md) | Sustained HTTP traffic with forced API/PostgreSQL crashes and recovery. |
| [Deployed OMS/Freight acceptance](native-acceptance.md) | Preflight and staging checks before connecting live services. |
| [Facility acceptance](facility-acceptance.md) | Signed physical acceptance with real devices and stock. |

## Releases and evidence

| Read | For |
|---|---|
| [Release qualification](release-qualification.md) | The complete release gate and its stage history. |
| [Evidence and limits](evidence.md) | What each dated run proved, what failed and what remains open. |
| [Open-source release checklist](open-source-release.md) | Licensing, source audit and publication steps. |
| [Changelog](../CHANGELOG.md) | Changes by version. |

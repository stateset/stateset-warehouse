# Public-source readiness — 2026-10-20

Source-release preparation has progressed, but launch remains **on hold**. The development candidate passes its software checks and source audit; its complete release gate fails serial crash recovery. The repository remains private.

Reviewed software commit: `93490ce2f09471c1b116575b460b77fc004f7e59`.
Tested immutable image: `sha256:bfa792fa32c3647221b0ee1a93100a1418c10330100a6a36eea00a78c1802046`.
The later evidence commit changes documentation only. Existing version tags and dated 0.2.0 qualification retain their original source and results.

## Recorded results

| Review | Result and scope |
|---|---|
| Software checks | 331 Rust, 90 Python, 101 terminal checks with zero skips, and 170 browser checks pass. |
| Complete local gate | **Failed.** Eight stages pass; `image_resilience` fails and `stable_source_inputs` does not run. See the [actual summary](qualification/source-candidate-20261020/summary.json). |
| Archive and restore | Passes with all 49 tables populated and exact restored hashes, 29 migrations, original-request replay, and a new reconciled write. Native external HTTP was not attempted. |
| Production container | Passes nonroot, read-only, dropped-capability runtime, readiness, authorization, MCP discovery, reconciliation and shutdown checks. This is a local container test, not production acceptance. |
| Untracked image resilience | Passes the 300-second, eight-operator, two-replica profile with 7,868 committed moves and 7,868 exact replays, no unexpected errors, crash recovery and independent reconciliation. Local synthetic measurements do not establish facility capacity. |
| Serial image resilience | **Failed** after deliberate PostgreSQL interruption: API database readiness did not return within the unchanged deadline. PostgreSQL was still in crash recovery. Sustained traffic and final independent serial proof did not complete. See the [recovery review](qualification/source-candidate-20261020/serial-readiness-review.json). |
| Source integrity | A separate post-failure check verifies all 1,267 candidate blobs/modes and all 444 unchanged software inputs. It does not stand in for the unexecuted final gate stage. See [verification](qualification/source-candidate-20261020/software-input-verification.json). |
| Source/history audit | All 33 available commits, exact candidate tree and commit metadata scanned with pinned, verified Gitleaks: zero unreviewed findings. Reviews bind original full-file bytes, path, rule and line range; no live-credential exception. See [source audit](qualification/source-candidate-20261020/source-audit.json). |
| Assets and dependencies | Synthetic screenshots and documented public RSA test keys reviewed; 169 upstream vendor files verified. Declared inventory covers 311 host Rust and 128 locked browser packages. Original notices for all eight browser runtime packages are present in the tested image. See [asset review](qualification/source-candidate-20261020/binary-assets.json), [third-party notices](../THIRD_PARTY_NOTICES.md), and [image proof](qualification/source-candidate-20261020/image-notices.json). |
| Fresh checkout | Locked Rust build, ordinary `npm ci`, TypeScript/Vite build and original notice comparison pass. Rust uses existing caches; default-port Compose was not started against shared host services. See [setup evidence](qualification/source-candidate-20261020/fresh-setup.json). |
| Hosted CI | The historical surface review found 29 jobs that executed no steps because of GitHub account billing/spending limits, with 29 empty downloadable log archives. It found no credential findings in the reviewed metadata/logs. A private push must be checked for its actual new hosted result. |

The [readiness record](qualification/source-candidate-20261020/readiness.json) and [artifact hashes](qualification/source-candidate-20261020/artifact-verification.json) bind the evidence. Only sanitized reports/transcripts are included. Original source archives, database data, images, traces, raw logs and failures remain retained privately.

## Failed attempts remain failed

| Attempt | Failure and follow-up |
|---|---|
| [First](qualification/source-candidate-20261020/prior-attempt-1-summary.json) | Final serial reconciliation exceeded the unchanged ten-second statement limit. The read-only query fix shares snapshot facts; full original proof comparisons and differential damage cases pass. See [query review](qualification/source-candidate-20261020/reconciliation-query-review.json). |
| [Second](qualification/source-candidate-20261020/prior-attempt-2-summary.json) | An ambiguous browser status selector failed during legitimate loading. A precise locator and deliberately held response pass all eight focused contracts. See [browser review](qualification/source-candidate-20261020/browser-test-review.json). |
| [Third](qualification/source-candidate-20261020/prior-attempt-3-summary.json) | A sleep-based terminal clock assertion raced scheduling. Controlled timer advancement passes all eight board tests. See [timer review](qualification/source-candidate-20261020/terminal-timer-review.json). |
| [Fourth](qualification/source-candidate-20261020/prior-attempt-4-summary.json) | Docker default address pools were exhausted. Explicit private subnet support passes eleven resource/network tests and actual Docker allocation. Existing networks remain retained. See [network review](qualification/source-candidate-20261020/qualification-network-review.json). |
| [Fifth](qualification/source-candidate-20261020/prior-attempt-5-summary.json) | On the current candidate, serial traffic returned HTTP 503 with warehouse row statement/lock timeouts after 395 moves were reported. Severe host stalls were observed afterward. See [write review](qualification/source-candidate-20261020/serial-write-review.json). |
| [Sixth/latest](qualification/source-candidate-20261020/summary.json) | A complete repeat on identical software passed eight stages and untracked resilience, then failed serial PostgreSQL recovery readiness. Severe host I/O/memory stalls were again observed. Sole causation remains unproven. |

No failure has been relabeled as a pass. The last repeat changed no software, write guards, migrations, journals, timeouts, retry budgets or traffic duration. Its three fresh isolated ranges are recorded in [network configuration](qualification/source-candidate-20261020/network-configuration.json).

## Remaining launch work

1. Run the entire [release procedure](release-qualification.md) on a runner with sufficient isolated resources. Require both full resilience profiles, serial identity proof, final reconciliation and the final unchanged-input stage to pass. Preserve these failures and the new run separately.
2. Obtain an actual hosted CI pass. Resolve the account runner restriction if it persists; historical metadata is not evidence of executed checks.
3. Review publication rights and delivery/monitoring of `security@stateset.com`. The technical review does not establish either. Follow [SECURITY.md](../SECURITY.md), [CONTRIBUTING.md](../CONTRIBUTING.md) and the [source-release checklist](open-source-release.md).
4. Receive an explicit public-launch instruction before changing repository visibility. Packages/images remain unpublished. Before official downloadable binaries or images, retain the applicable Rust and container system-package notices for the actual targets/options being shipped.

Pattern scanning and visual review do not guarantee the absence of confidential material. Deployed identity, real OMS/Freight and MCP clients, native Ghostty/scanner and signed facility acceptance remain separate. `source_launch_ready=false`; `production_qualified=false`.

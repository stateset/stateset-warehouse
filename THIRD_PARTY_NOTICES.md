# Third-party notices

StateSet Warehouse is available under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. The root [LICENSE](LICENSE) records that choice. Dependency licenses remain their respective authors' licenses.

The repository includes unmodified StateSet iCommerce 1.39.0 crate sources from public upstream commit `b8a5c6db885998defa9fd74e2b12f0d1add21e1c`:

- `stateset-primitives`: typed SKU and identifier primitives.
- `stateset-core`: domain inputs, validation and inventory projection types.
- `stateset-crypto`: transitive core dependency, including upstream cryptographic models.
- `stateset-test-utils`: upstream development support; not a WMS runtime inventory engine.

These sources are copyright 2026 StateSet and dual licensed MIT OR Apache-2.0. Their original [license](vendor/stateset-icommerce/LICENSE), [MIT text](vendor/stateset-icommerce/LICENSE-MIT), [Apache text](vendor/stateset-icommerce/LICENSE-APACHE), package manifests and source notices are retained. [UPSTREAM.json](vendor/stateset-icommerce/UPSTREAM.json) records every vendored file's Git blob and SHA256 hash. The consumer workspace manifest is normalized for portable local path dependencies; the separate upstream crypto fuzz workspace is excluded. No crate source is patched.

Other Rust dependencies are resolved in [Cargo.lock](Cargo.lock); their package metadata contains individual license declarations. Browser dependencies and exact versions are recorded in [dashboard/package-lock.json](dashboard/package-lock.json). The terminal application uses Node.js built-in modules and has no third-party runtime npm dependencies. Container base images and system packages retain their upstream licenses and require separate distribution review with each published image.

The built dashboard carries [original browser runtime notices](dashboard/public/THIRD_PARTY_NOTICES.txt) at `/THIRD_PARTY_NOTICES.txt`. This file retains the license and copyright texts of all eight locked runtime packages, including Lucide's Feather attribution. After changing browser dependencies, run `npm --prefix dashboard ci`, then `python3 scripts/dashboard_notices.py` and review the diff. `scripts/check.sh` rejects absent/stale notices, changed installed versions or license declarations, and missing original texts. Build dependencies are inventoried separately by the source preflight; this browser file is not a notice bundle for Rust binaries or container system packages.

Do not infer that all transitive dependencies share the root project's license. Before a public source, package or image release, review the resolved dependency inventory and included notices for that actual artifact. See [open-source release preparation](docs/open-source-release.md).

The terminal's [Code 128 pattern table](cli/lib/code128.mjs) is extracted from ZXing `Code128Reader.java`, tag `zxing-3.5.3`, copyright 2008 ZXing authors, licensed Apache-2.0. The original attribution, source link and modification notice ship in [CODE128-NOTICE.txt](cli/lib/CODE128-NOTICE.txt); the complete Apache license ships with the terminal. StateSet supplies its own Set B encoder and raster renderer. ZXing is not a terminal runtime dependency.

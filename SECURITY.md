# Security reporting

Report suspected vulnerabilities privately to **security@stateset.com**. Include the affected version or commit, a minimal reproduction with synthetic data, and the expected impact. Do not post credentials, customer records, warehouse dumps or exploit details in public issues. Never test against facilities or services you do not own or have permission to assess.

The project currently has one development line. A software qualification report applies to its recorded source and image; it does not establish a supported production deployment. Security fixes should be reviewed on the current line before a new release is qualified.

Warehouse writes require an authenticated tenant and actor, canonical request identity, reviewed versions, and actual physical observations where required. Preserve complete original requests through uncertain outcomes. A missing receipt or HTTP 503 does not prove a write was rejected. See [authentication](docs/authentication.md), [API recovery](docs/api.md), and [terminal recovery](cli/README.md).

Use individual operator credentials, HTTPS for remote access, private recovery files, a secret manager, isolated test databases and tested backups. Keep `.env`, `.local`, database dumps and local qualification artifacts out of public releases. The repository includes synthetic public evidence; operational data must remain private.

# Production deployment kit

Runs StateSet Warehouse on one Linux host with Docker, behind Caddy (automatic HTTPS), against a managed PostgreSQL 16. It includes verified daily backups, a weekly restore drill and optional Prometheus monitoring. Read [deploying to production](../../docs/production.md) first; this kit implements those steps.

| File | Purpose |
|---|---|
| `compose.yml` | App (hardened: read-only, no capabilities, UID 10001) and Caddy; Prometheus with `--profile monitoring` |
| `Caddyfile` | HTTPS for `WMS_DOMAIN`, HSTS, and keeps `/v1/operations/metrics` off the public internet |
| `deploy.sh <tag>` | Builds the exact tag, backs up first, starts it, waits for `/ready`, logs what was deployed |
| `backup.sh` | `pg_dump` custom-format backup with SHA-256 and an archive check; keeps 14 days |
| `restore-drill.sh` | Restores the latest backup into a scratch database, starts the release against it and requires every warehouse to reconcile |
| `systemd/` | Daily backup and weekly restore-drill timers |
| `env/*.example` | The four settings files that live in `/etc/stateset-wms/` |

## 1. One-time server setup

1. Install Docker Engine with the Compose plugin, `git`, `curl` and `python3`.
2. Point the DNS record for your domain at the host, and open ports 80 and 443 only.
3. Clone the repository to `/opt/stateset-warehouse`.
4. Create `/etc/stateset-wms/` (mode 0700, owner root) with these files, each mode 0600, using `env/*.example` as templates:
   - `database.env`: `DATABASE_URL` for the managed database, with `sslmode=require`;
   - `app.env`: API keys and optional sign-in and integration settings;
   - `deploy.env`: `WMS_DOMAIN`;
   - `drill.env`: a role allowed to create and drop scratch databases, plus an admin API key;
   - `metrics.token` (only with monitoring): an admin API key on one line.
5. Install the timers:
   ```bash
   sudo cp /opt/stateset-warehouse/deploy/production/systemd/* /etc/systemd/system/
   sudo systemctl daemon-reload
   sudo systemctl enable --now stateset-wms-backup.timer stateset-wms-restore-drill.timer
   ```

## 2. Deploy a release

```bash
cd /opt/stateset-warehouse
sudo bash deploy/production/deploy.sh v0.3.0
```

The script refuses a checkout with local changes. It takes and verifies a backup **before** starting the new version, and appends the tag, commit, image ID and previous release to `/var/lib/stateset-wms/deployments.log`.

## 3. Verify

1. `https://WMS_DOMAIN/ready` returns 200.
2. Sign in, then run `GET /v1/warehouses/{id}/reconciliation` for each warehouse. Every report must have `"ok": true`.
3. Run `sudo bash deploy/production/restore-drill.sh` once by hand. It must end with "Restore drill passed".

## 4. Monitoring

Start Prometheus alongside the app with:
```bash
WMS_RELEASE=$(cat /var/lib/stateset-wms/current-release) WMS_DOMAIN=... \
  docker compose -f deploy/production/compose.yml --profile monitoring up -d
```
It scrapes the admin metrics endpoint inside the compose network and loads the tested rules in `deploy/monitoring/alerts.yml`. Route its alerts to a person (Alertmanager, or your existing pager), and check that a test alert arrives.

## 5. Upgrades and rollback

- **Upgrade:** pause operators and integrations, then run `deploy.sh` with the new tag, verify (step 3) and resume.
- **Rolling back is only safe if the newer release added no migrations.** Check the changelog for new migrations before rolling back. Releases up to 0.3.x do not themselves refuse a newer schema; a startup check that does is planned. If migrations were added, roll back by **restoring the pre-upgrade backup** that `deploy.sh` took into a fresh database, pointing `database.env` at it, then deploying the previous tag. Any physical work recorded after the backup must then be re-entered from its paper or scanner records. See [operations](../../docs/operations.md#back-up-and-restore-into-a-fresh-database).

## 6. If reconciliation reports `ok: false`

Stop the affected work and keep the evidence: export the reconciliation report and keep the database as it is. Then follow [resolve delivery and inventory incidents](../../docs/operations.md#resolve-delivery-and-inventory-incidents). Never "fix" rows with direct SQL; the database guards are there to stop exactly that.

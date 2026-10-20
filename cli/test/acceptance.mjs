import { accessSync, constants, readdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const database = process.env.WMS_TEST_DATABASE_URL;
if (!database || !new URL(database).pathname.includes('test')) throw new Error('Set WMS_TEST_DATABASE_URL to an explicitly isolated PostgreSQL test database; acceptance cannot skip the live checks.');
try { accessSync(fileURLToPath(new URL('../../target/debug/stateset-warehouse', import.meta.url)), constants.X_OK); }
catch { throw new Error('Build the local API first: cargo build --bin stateset-warehouse'); }
const directory = fileURLToPath(new URL('.', import.meta.url));
const files = readdirSync(directory).filter(name => name.endsWith('.test.mjs')).sort().map(name => `${directory}${name}`);
const result = spawnSync(process.execPath, ['--test', ...files], { stdio: 'inherit', env: process.env });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;

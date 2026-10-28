#!/usr/bin/env python3
"""Exercise actual HTTP replicas and owned PostgreSQL; Python standard library only."""
import argparse
import collections
import concurrent.futures
import hashlib
import http.client
import http.server
import json
import math
import pathlib
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request
import urllib.parse

from serial_traffic_proof import serial_proof
from serial_crash_barrier import barrier_sql, literal
from crash_journal import CrashJournal
from qualification_host import host_snapshot, write_report
import traffic_policy
import uuid


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def latency_summary(values):
    ordered = sorted(values)
    return (
        {
            "samples": len(ordered),
            **{
                f"p{p}_ms": round(
                    ordered[max(0, math.ceil(len(ordered) * p / 100) - 1)], 3
                )
                for p in (50, 95, 99)
            },
            "max_ms": round(ordered[-1], 3),
        }
        if ordered
        else {"samples": 0}
    )


class Probe:
    tenant = "resilience-test"
    route = "/v1/warehouses/wh-main"
    label = "org.stateset.wms.resilience"

    def __init__(self, args):
        self.args = args
        self.root = pathlib.Path(args.artifacts)
        self.credentials = json.loads((self.root / "credentials.json").read_text())
        self.admin = next(
            k
            for k, v in self.credentials.items()
            if v["actor"] == "qualification:admin"
        )
        self.operators = sorted(
            (v["actor"], k)
            for k, v in self.credentials.items()
            if v["role"] == "operator"
        )
        self.outsider = next(
            k for k, v in self.credentials.items() if v["tenant"] != self.tenant
        )
        self.bases = []
        self.accepted = {}
        self.phase_evidence = {}
        self.started = time.time()
        self.qualifier_hashes = self.script_hashes()
        self.seeded_orders = []
        self.serial_recovery_requests = []
        for container in [args.postgres, *args.api]:
            require(
                re.fullmatch(
                    r"wms-resilience-(?:pg|api-[ab])-" + re.escape(args.run), container
                ),
                "Unexpected resource name",
            )
            self.owned(container)
        self.refresh_bindings()

    @staticmethod
    def script_hashes():
        return {
            name: hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest()
            for name in (
                "scripts/resilience-rehearsal.sh",
                "scripts/qualification_resources.sh",
                "scripts/qualification_host.py",
                "scripts/resilience_probe.py",
                "scripts/serial_traffic_proof.py",
                "scripts/traffic_policy.py",
                "scripts/serial_crash_barrier.py",
                "scripts/crash_journal.py",
            )
        }

    def bin(self, index):
        return f"BIN-{index % self.args.bins:05}"

    @staticmethod
    def sku(index):
        return f"LOAD-{index:05}"

    @staticmethod
    def lot(index):
        return f"LOT-{index:03}"

    def opening_quantity(self):
        return getattr(self.args, "serial_units", 0) or 100000

    @staticmethod
    def serial_id(index, lot, unit):
        return f"Load-{index:05}-Lot-{lot:03}-Unit-{unit:05}"

    def expected_setup_commands(self):
        # Storage bins; recovery product/receipt; operational products/counts;
        # open orders and every other order's one allocation.
        return (
            self.args.bins
            + 2
            + self.args.products * (self.args.lots + 1)
            + self.args.open_orders
            + (self.args.open_orders + 1) // 2
        )

    def refresh_bindings(self):
        self.bases = []
        for container in self.args.api:
            binding = self.docker("port", container, "8088/tcp").strip()
            require(
                re.fullmatch(r"127\.0\.0\.1:[0-9]+", binding),
                "HTTP must bind only random loopback port",
            )
            self.bases.append("http://" + binding)

    @staticmethod
    def docker(*args, timeout=90):
        return subprocess.run(
            ["docker", *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=timeout,
        ).stdout

    def owned(self, container, timeout=90):
        metadata = json.loads(self.docker("inspect", container, timeout=timeout))[0]
        require(
            metadata["Config"]["Labels"].get(self.label) == self.args.run,
            "Resource ownership label mismatch",
        )
        return metadata

    def sql(self, sql):
        result = subprocess.run(
            [
                "docker",
                "exec",
                "-i",
                self.args.postgres,
                "psql",
                "-X",
                "-qAt",
                "-v",
                "ON_ERROR_STOP=1",
                "-U",
                "warehouse",
                "-d",
                "warehouse_resilience_test",
            ],
            input=sql,
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
        )
        return result.stdout.strip()

    def table(self, name):
        require(re.fullmatch(r"wms_[a-z_]+", name), "Unsafe table identifier")
        return json.loads(
            self.sql(
                f"SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]') FROM {name} t;"
            )
        )

    def http(self, base, method, path, token=None, body=None, timeout=12):
        request = urllib.request.Request(
            base + path,
            data=None
            if body is None
            else json.dumps(body, separators=(",", ":")).encode(),
            method=method,
        )
        if token:
            request.add_header("Authorization", "Bearer " + token)
        request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                data = response.read()
                return response.status, json.loads(data) if data else None
        except urllib.error.HTTPError as error:
            data = error.read()
            try:
                data = json.loads(data)
            except (ValueError, UnicodeDecodeError):
                data = data.decode(errors="replace")
            return error.code, data

    def reconciliation_run(self, base, seconds=600):
        """Run the complete proof in the background on this replica and poll it.

        Each request keeps the 12-second HTTP limit; only the proof itself runs
        longer, under the run's own statement deadline. Returns (proof, run).
        """
        status, run = self.http(
            base, "POST", self.route + "/reconciliation/runs", self.admin
        )
        require(
            status == 202 and run.get("status") in ("queued", "running"),
            "API reconciliation run was not accepted",
        )
        path = self.route + "/reconciliation/runs/" + urllib.parse.quote(
            run["id"], safe=""
        )
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            status, run = self.http(base, "GET", path, self.admin)
            require(status == 200, "API reconciliation run could not be read")
            if run["status"] == "succeeded":
                return run["result"], run
            require(
                run["status"] != "failed",
                "API full ledger reconciliation failed: " + str(run.get("error")),
            )
            time.sleep(0.5)
        raise AssertionError("API reconciliation run did not finish in time")

    def wait(self, condition, description, seconds=60):
        deadline = time.monotonic() + seconds
        last = None
        while time.monotonic() < deadline:
            try:
                last = condition()
                if last:
                    return last
            except (
                OSError,
                subprocess.SubprocessError,
                urllib.error.URLError,
                http.client.HTTPException,
            ):
                pass
            time.sleep(0.1)
        raise AssertionError(
            f"Timed out waiting for {description}; last result: {last}"
        )

    def ready(self):
        for replica, base in enumerate(self.bases):
            last_status = None

            def available():
                nonlocal last_status
                last_status = self.http(base, "GET", "/ready", timeout=7)[0]
                return last_status == 200

            try:
                self.wait(available, "API database readiness", 60)
            except AssertionError:
                try:
                    self.record_readiness_failure(replica, last_status)
                except Exception as error:
                    # Disk exhaustion or failed diagnostics cannot hide the
                    # original failed recovery or turn it into a passing test.
                    print("Readiness diagnostics unavailable: " + type(error).__name__, flush=True)
                raise

    def record_readiness_failure(self, replica, last_status):
        report = {'replica_index': replica, 'last_http_status': last_status,
                  'readiness_deadline_seconds': 60, 'result': 'failed',
                  'host': host_snapshot({'workspace': pathlib.Path.cwd(),
                                         'artifacts': self.root, 'temporary': pathlib.Path('/tmp')})}
        try:
            state = self.owned(self.args.postgres, timeout=10)['State']
            report['postgres'] = {key: state.get(key) for key in ('Status', 'Running', 'ExitCode', 'OOMKilled')}
            report['postgres']['health_status'] = state.get('Health', {}).get('Status')
        except Exception as error:
            report['postgres_observation_error'] = type(error).__name__
        if 'postgres' in report:
            try:
                log = subprocess.run(['docker', 'logs', '--timestamps', '--tail', '80', self.args.postgres],
                                     check=True, capture_output=True, text=True, timeout=10)
                # PostgreSQL writes important recovery messages to stderr. This
                # private report is never included in the publish allowlist.
                report['postgres_log_tail'] = log.stdout + log.stderr
            except Exception as error:
                report['postgres_log_error'] = type(error).__name__
        else:
            report['postgres_log_error'] = 'OwnershipNotVerified'
        write_report(self.root / 'readiness-failure.json', report)

    def version(self, base=None, token=None):
        status, data = self.http(
            base or self.bases[0], "GET", "/v1/warehouses", token or self.admin
        )
        require(status == 200, f"Warehouse metadata failed: {status} {data}")
        return next(
            item["version"]
            for item in data["warehouses"]
            if item["warehouse"]["id"] == "wh-main"
        )

    def snapshot_review(self, base=None, token=None):
        started = time.monotonic()
        status, snapshot = self.http(
            base or self.bases[0], "GET", self.route, token or self.admin
        )
        require(
            status == 200 and snapshot["reconciliation"]["ok"] is True,
            "Operational snapshot failed reconciliation during sustained traffic",
        )
        self.check_snapshot_coverage(snapshot)
        return snapshot, (time.monotonic() - started) * 1000

    def check_snapshot_coverage(self, snapshot):
        require(snapshot.get("warehouse", {}).get("id") == "wh-main",
                "Operational snapshot belongs to another warehouse")
        expected_skus = {"PROBE"} | {self.sku(i) for i in range(self.args.products)}
        require(len(snapshot["products"]) == len(expected_skus)
                and {p["sku"] for p in snapshot["products"]} == expected_skus,
                "Operational snapshot lost or substituted catalog records")
        orders = {o["id"]: o for o in snapshot["orders"]}
        require(len(orders) == len(snapshot["orders"])
                and len(self.seeded_orders) == self.args.open_orders
                and all(order_id in orders and orders[order_id]["status"] == ("allocated" if i % 2 == 0 else "pending")
                        for i, order_id in enumerate(self.seeded_orders)),
                "Operational snapshot lost or substituted active order identities/states")

    def envelope(self, command, base=None, token=None):
        return {
            "request_id": str(uuid.uuid4()),
            "expected_version": self.version(base, token),
            "command": command,
        }

    def retain(self, request, result, token):
        actor = self.credentials[token]["actor"]
        previous = self.accepted.setdefault(
            request["request_id"],
            {"request": request, "result": result, "actor": actor},
        )
        require(
            previous == {"request": request, "result": result, "actor": actor},
            "A committed request changed identity/result",
        )

    def submit(self, command, token=None, base=None, serial_inputs=None):
        token = token or self.admin
        base = base or self.bases[0]
        request = self.envelope(command, base, token)
        if serial_inputs is not None:
            request["serial_inputs"] = serial_inputs
        status, data = self.http(base, "POST", self.route + "/commands", token, request)
        require(
            status == 200 and data["replayed"] is False,
            f"Command failed: {status} {data}",
        )
        self.retain(request, data["result"], token)
        return data["result"]

    def replay(self, request, token, base):
        status, data = self.http(base, "POST", self.route + "/commands", token, request)
        require(
            status == 200 and data["replayed"] is True,
            f"Exact replay failed: {status} {data}",
        )
        self.retain(request, data["result"], token)
        return data["result"]

    def seed(self):
        seed_started = time.monotonic()
        self.ready()
        for base in self.bases:
            require(
                self.http(base, "GET", "/v1/warehouses")[0] == 401,
                "Anonymous API accepted",
            )
            for token, identity in self.credentials.items():
                status, session = self.http(base, "GET", "/v1/session", token)
                require(
                    status == 200
                    and session["demo"] is False
                    and session["identity"] == identity,
                    "Real operator identity mismatch",
                )
        print(
            f"Seeding {self.args.products} products, {self.args.products * self.args.lots} opening bin-lots and {self.args.open_orders} open orders through authenticated HTTP.",
            flush=True,
        )
        for code in (self.bin(i) for i in range(self.args.bins)):
            self.submit(
                {
                    "type": "create_location",
                    "code": code,
                    "zone": "Qualification",
                    "kind": "storage",
                }
            )
        self.submit(
            {
                "type": "create_product",
                "sku": "PROBE",
                "name": "Recovery evidence product",
                "category": "Qualification",
                "reorder_point": 0,
            }
        )
        result = self.submit(
            {
                "type": "create_receipt",
                "reference": "RESILIENCE-RECEIPT",
                "supplier": "Synthetic supplier",
                "lines": [
                    {
                        "id": "same-sku-first",
                        "sku": "PROBE",
                        "lot": "LOT-A",
                        "quantity": 7,
                    },
                    {
                        "id": "same-sku-second",
                        "sku": "PROBE",
                        "lot": "LOT-A",
                        "quantity": 11,
                    },
                ],
            }
        )
        self.receipt_id = result["entity_id"]
        for i in range(self.args.products):
            self.submit(
                {
                    "type": "create_product",
                    "sku": self.sku(i),
                    "name": f"Operational product {i}",
                    **({"serial_controlled": True} if getattr(self.args, "serial_units", 0) else {}),
                    "category": "Qualification",
                    "reorder_point": 0,
                }
            )
            for lot in range(self.args.lots):
                self.submit(
                    {
                        "type": "count_stock",
                        "sku": self.sku(i),
                        "lot": self.lot(lot),
                        "location": self.bin(i),
                        "counted": self.opening_quantity(),
                        "reason": "Synthetic opening balance observed by qualifier",
                    },
                    serial_inputs=[{"target": [], "sku": self.sku(i),
                                    "serials": [self.serial_id(i, lot, n) for n in range(self.args.serial_units)]}]
                    if getattr(self.args, "serial_units", 0) else None,
                )
            if (i + 1) % 100 == 0:
                print(
                    f"Seeded {i + 1} stocked products in {time.monotonic() - seed_started:.1f}s.",
                    flush=True,
                )
        for i in range(self.args.open_orders):
            order = self.submit(
                {
                    "type": "create_order",
                    "reference": f"RESILIENCE-OPEN-{i:05}",
                    "source_order_id": None,
                    "customer": "Synthetic active-work customer",
                    "destination": "Qualification facility",
                    "priority": "normal",
                    "lines": [
                        {
                            "id": "line-1",
                            "sku": self.sku(i % self.args.products),
                            "quantity": 1,
                        }
                    ],
                }
            )
            self.seeded_orders.append(order["entity_id"])
            if i % 2 == 0:
                self.submit({"type": "allocate", "order_id": order["entity_id"]})
            if (i + 1) % 100 == 0:
                print(
                    f"Seeded {i + 1} active orders in {time.monotonic() - seed_started:.1f}s.",
                    flush=True,
                )
        expected_commands = self.expected_setup_commands()
        require(
            len(self.accepted) == self.version() == expected_commands,
            "Seed command receipts/version do not match independent fixture arithmetic",
        )
        self.phase_evidence["seed"] = {
            "method": "authenticated actual HTTP commands only; opening counts are disclosed synthetic fixtures",
            "seconds": round(time.monotonic() - seed_started, 3),
            "operational_products": self.args.products,
            "additional_recovery_product": 1,
            "storage_bins": self.args.bins,
            "lots_per_product": self.args.lots,
            "opening_bin_lot_balances": self.args.products * self.args.lots,
            "opening_each_per_balance": self.opening_quantity(),
            "serial_units_per_bin_lot": getattr(self.args, "serial_units", 0),
            "open_orders": self.args.open_orders,
            "allocated_orders": (self.args.open_orders + 1) // 2,
            "commands_before_recovery_lifecycle": len(self.accepted),
            "expected_setup_commands": expected_commands,
            "observed_setup_commands": len(self.accepted),
            "independent_setup_command_count_matches": True,
            "excluded_from_sustained_timing": True,
        }

    def lose_success(self):
        print(
            "Qualifying a deliberately lost HTTP success through the other API replica.",
            flush=True,
        )
        token = self.operators[0][1]
        request = self.envelope(
            {
                "type": "receive",
                "receipt_id": self.receipt_id,
                "line_id": "same-sku-first",
                "quantity": 7,
                "location": "RECV-01",
            },
            token=token,
        )
        probe = self
        observed = {}

        class DropSuccess(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                try:
                    body = json.loads(
                        self.rfile.read(int(self.headers["Content-Length"]))
                    )
                    status, data = probe.http(
                        probe.bases[0], "POST", probe.route + "/commands", token, body
                    )
                    require(
                        status == 200 and data["replayed"] is False,
                        "Proxy upstream did not commit once",
                    )
                    observed.update(
                        status=status,
                        result=data["result"],
                        request_digest=digest(body),
                    )
                except (
                    Exception
                ) as error:  # Preserve proxy-thread failures for the caller.
                    observed["error"] = repr(error)
                finally:
                    self.close_connection = (
                        True  # Intentionally send no downstream status/body.
                    )

            def log_message(self, *_args):
                pass

        proxy = http.server.ThreadingHTTPServer(("127.0.0.1", 0), DropSuccess)
        thread = threading.Thread(target=proxy.serve_forever, daemon=True)
        thread.start()
        lost = False
        try:
            self.http(
                f"http://127.0.0.1:{proxy.server_address[1]}",
                "POST",
                "/discard-success",
                token,
                request,
            )
        except (urllib.error.URLError, http.client.HTTPException, OSError):
            lost = True
        finally:
            proxy.shutdown()
            proxy.server_close()
            thread.join(timeout=5)
        require(
            lost and observed.get("status") == 200 and "error" not in observed,
            f"Lost-success proof failed: {observed}",
        )
        self.retain(request, observed["result"], token)
        result = self.replay(request, token, self.bases[1])
        require(
            result == observed["result"], "Response-loss retry changed original result"
        )
        status, receipt = self.http(
            self.bases[1], "GET", self.route + "/receipts/" + self.receipt_id, token
        )
        require(
            status == 200
            and {line["id"]: line["received"] for line in receipt["lines"]}
            == {"same-sku-first": 7, "same-sku-second": 0},
            "Response-loss replay changed another same-SKU receipt line",
        )
        self.phase_evidence["lost_http_success"] = {
            "downstream_response_lost": True,
            "upstream_commit_observed_before_drop": True,
            "retry_other_replica": True,
            "original_result_unchanged": True,
            "exact_receipt_lines": {"same-sku-first": 7, "same-sku-second": 0},
        }

    def lifecycle(self):
        token = self.operators[0][1]
        self.submit(
            {
                "type": "receive",
                "receipt_id": self.receipt_id,
                "line_id": "same-sku-second",
                "quantity": 11,
                "location": "RECV-01",
            },
            token,
        )
        self.submit(
            {
                "type": "move_stock",
                "sku": "PROBE",
                "lot": "LOT-A",
                "from": "RECV-01",
                "to": self.bin(0),
                "quantity": 18,
            },
            token,
        )
        result = self.submit(
            {
                "type": "create_order",
                "reference": "RESILIENCE-SHIP",
                "source_order_id": None,
                "customer": "Synthetic customer",
                "destination": "Synthetic receiving dock",
                "priority": "normal",
                "lines": [
                    {"id": "same-sku-first", "sku": "PROBE", "quantity": 2},
                    {"id": "same-sku-second", "sku": "PROBE", "quantity": 3},
                ],
            },
            token,
        )
        self.shipped_order = result["entity_id"]
        self.submit({"type": "allocate", "order_id": self.shipped_order}, token)
        for line, quantity in (("same-sku-first", 2), ("same-sku-second", 3)):
            self.submit(
                {
                    "type": "pick",
                    "order_id": self.shipped_order,
                    "line_id": line,
                    "location": self.bin(0),
                    "lot": "LOT-A",
                    "quantity": quantity,
                },
                token,
            )
        self.submit({"type": "pack", "order_id": self.shipped_order}, token)
        self.submit(
            {
                "type": "ship",
                "order_id": self.shipped_order,
                "carrier": "Synthetic carrier",
                "handoff_reference": "SYNTHETIC-ONLY-001",
            },
            token,
        )
        result = self.submit(
            {
                "type": "create_order",
                "reference": "RESILIENCE-RESERVED",
                "source_order_id": None,
                "customer": "Synthetic customer",
                "destination": "Synthetic dock",
                "priority": "normal",
                "lines": [{"id": "held-line", "sku": "PROBE", "quantity": 3}],
            },
            token,
        )
        self.reserved_order = result["entity_id"]
        self.submit({"type": "allocate", "order_id": self.reserved_order}, token)

    def recovery_request(self, returning, token):
        serial = getattr(self.args, "serial_units", 0)
        index = self.args.products - 1 if serial else 0
        request = self.envelope(
            {"type": "move_stock", "sku": self.sku(index) if serial else "PROBE",
             "lot": self.lot(0) if serial else "LOT-A",
             "from": self.bin(index + int(returning)),
             "to": self.bin(index + int(not returning)), "quantity": 1},
            token=token,
        )
        if serial:
            request["serial_inputs"] = [{"target": [], "sku": self.sku(index),
                                         "serials": [self.serial_id(index, 0, serial - 1)]}]
        return request

    def locker(self, request=None, token=None):
        application = "wms-resilience-locker-" + self.args.run
        statement = "BEGIN; SELECT version FROM wms_warehouses WHERE tenant='resilience-test' AND warehouse_id='wh-main' FOR UPDATE; SELECT pg_sleep(120); COMMIT;\n"
        if getattr(self.args, "serial_units", 0):
            command = request["command"]
            sku, lot = literal(command["sku"]), literal(command["lot"])
            label = literal(request["serial_inputs"][0]["serials"][0])
            baseline = json.loads(self.sql(f"""SELECT jsonb_build_object(
                'revision',u.revision,'head',h.version,
                'source_quantity',COALESCE((SELECT on_hand FROM wms_stock WHERE tenant=u.tenant
                    AND warehouse_id='wh-main' AND sku=u.sku AND lot=u.lot AND location={literal(command['from'])}),0),
                'destination_quantity',COALESCE((SELECT on_hand FROM wms_stock WHERE tenant=u.tenant
                    AND warehouse_id='wh-main' AND sku=u.sku AND lot=u.lot AND location={literal(command['to'])}),0))
                FROM wms_serial_units u JOIN wms_serial_heads h USING(tenant,sku)
                WHERE u.tenant='resilience-test' AND u.sku={sku} AND u.lot={lot} AND u.serial={label}
                AND u.place=wms_serial_on_hand('wh-main',{literal(command['from'])});"""))
            keys = (180121, int(uuid.uuid4().hex[:7], 16) + 1)
            name = "wms_resilience_serial_" + self.args.run.replace("-", "_")
            marker = "wms-serial-after-write-" + self.args.run
            sql = barrier_sql(name, marker, request, self.credentials[token]["actor"], baseline, keys)
            self.crash_journal.record("barrier_planned", baseline=baseline, sql=sql,
                                      name=name, marker=marker, keys=keys)
            self.sql(sql)
            self.crash_journal.record("barrier_installed")
            self.serial_barrier = {"name": name, "marker": marker, "keys": keys,
                                   "original_request": request, "actor": self.credentials[token]["actor"],
                                   "before": baseline}
            statement = f"SELECT pg_advisory_lock({keys[0]},{keys[1]}); SELECT pg_sleep(120);\n"
        process = subprocess.Popen(
            [
                "docker",
                "exec",
                "-i",
                "--env",
                "PGAPPNAME=" + application,
                self.args.postgres,
                "psql",
                "-X",
                "-qAt",
                "-v",
                "ON_ERROR_STOP=1",
                "-U",
                "warehouse",
                "-d",
                "warehouse_resilience_test",
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        process.stdin.write(statement)
        process.stdin.flush()
        self.wait(
            lambda: self.sql(
                f"SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name='{application}' AND wait_event='PgSleep');"
            )
            == "t",
            "owned row-lock holder",
        )
        return application, process

    def wait_blocked(self, application):
        if getattr(self.args, "serial_units", 0):
            barrier = self.serial_barrier
            first, second = barrier["keys"]
            query = f"""SELECT COALESCE(jsonb_agg(jsonb_build_object(
                'writer_pid',a.pid,'blocker_pid',b.pid,'writer_application',a.application_name,
                'wait_event',a.wait_event,'lock_classid',l.classid,'lock_objid',l.objid)), '[]')
                FROM pg_stat_activity a JOIN pg_locks l ON l.pid=a.pid AND NOT l.granted
                JOIN pg_stat_activity b ON b.pid=ANY(pg_blocking_pids(a.pid))
                JOIN pg_locks holder ON holder.pid=b.pid AND holder.granted
                    AND holder.locktype=l.locktype AND holder.database=l.database
                    AND holder.classid=l.classid AND holder.objid=l.objid AND holder.objsubid=l.objsubid
                WHERE b.application_name={literal(application)} AND a.application_name={literal(barrier['marker'])}
                AND a.wait_event_type='Lock' AND a.wait_event='advisory'
                AND l.locktype='advisory' AND l.classid={first} AND l.objid={second} AND l.objsubid=2;"""
            evidence = self.wait(lambda: json.loads(self.sql(query)),
                                 "exact serial HTTP transaction after custody/event/receipt mutations")
            require(len(evidence) == 1, "More than one writer reached the owned serial barrier")
            barrier["blocking_evidence"] = evidence[0]
            self.crash_journal.record("writer_blocked", evidence=evidence[0])
            return
        self.wait(
            lambda: self.sql(
                f"SELECT EXISTS(SELECT 1 FROM pg_stat_activity a JOIN pg_stat_activity b ON b.pid=ANY(pg_blocking_pids(a.pid)) WHERE b.application_name='{application}' AND a.query LIKE '%wms_warehouses%' AND a.pid<>b.pid);"
            )
            == "t",
            "actual HTTP write blocked behind row lock",
        )
        self.crash_journal.record("writer_blocked", warehouse_row_lock=True)

    def remove_serial_barrier(self):
        if getattr(self.args, "serial_units", 0):
            name = self.serial_barrier["name"]
            self.sql(f"DROP TRIGGER {name} ON wms_outbox; DROP FUNCTION {name}();")
            self.serial_barrier["owned_fault_trigger_removed"] = True
            self.crash_journal.record("barrier_removed", name=name)

    def crash_barrier_evidence(self):
        if not getattr(self.args, "serial_units", 0):
            return {"barrier_stage": "before_inventory_mutations_warehouse_row_lock"}
        require(self.serial_barrier.get("owned_fault_trigger_removed") is True,
                "Owned serial fault trigger remains installed")
        return {"barrier_stage": "after_serial_custody_history_event_receipt_before_outbox_commit",
                "serial_after_write_barrier": self.serial_barrier.copy()}

    def fingerprints(self):
        tables = self.sql(
            "SELECT tablename FROM pg_tables WHERE schemaname='public' AND tablename LIKE 'wms_%' ORDER BY tablename;"
        ).splitlines()
        return {
            name: {"rows": len(rows), "sha256": digest(rows)}
            for name in tables
            for rows in [self.table(name)]
        }

    @staticmethod
    def consume_future(future):
        try:
            status, _data = future.result(timeout=20)
            require(
                status == 503,
                f"Interrupted request unexpectedly returned HTTP {status}",
            )
            return {"http_status": status}
        except (urllib.error.URLError, OSError, http.client.HTTPException) as error:
            return {"transport_error": type(error).__name__}

    def crash_api(self):
        print(
            "Killing one API only after PostgreSQL proves its original HTTP write is waiting.",
            flush=True,
        )
        token = self.operators[0][1]
        request = self.recovery_request(False, token)
        self.crash_journal = CrashJournal(self.root, "api_sigkill", self.args.run,
                                          self.credentials[token]["actor"], request)
        before = self.fingerprints() if getattr(self.args, "serial_units", 0) else None
        self.crash_journal.record("baseline_captured", fingerprints=before)
        application, locker = self.locker(request, token)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            self.crash_journal.record("original_send_requested", replica=0)
            pending = pool.submit(
                self.http,
                self.bases[0],
                "POST",
                self.route + "/commands",
                token,
                request,
                20,
            )
            try:
                self.wait_blocked(application)
                self.owned(self.args.api[0])
                self.crash_journal.record("fault_requested", container=self.args.api[0], signal="KILL")
                self.docker("kill", "--signal=KILL", self.args.api[0])
                interrupted = self.consume_future(pending)
                self.crash_journal.record("interrupted_response_observed", response=interrupted)
                require(
                    self.owned(self.args.api[0])["State"]["ExitCode"] == 137,
                    "API was not abruptly killed",
                )
                self.crash_journal.record("fault_verified", exit_code=137)
                require(
                    self.http(self.bases[1], "GET", "/ready")[0] == 200,
                    "Surviving API lost readiness",
                )
            finally:
                self.sql(
                    f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name='{application}';"
                )
                locker.communicate(timeout=10)
        self.remove_serial_barrier()
        if before is not None:
            require(self.fingerprints() == before,
                    "Serial API crash did not roll back all committed table fingerprints")
            self.crash_journal.record("rollback_verified")
        status, receipt = self.http(
            self.bases[1],
            "GET",
            self.route + "/commands/" + request["request_id"],
            token,
        )
        require(
            status == 200 and receipt == {"found": False},
            "Killed blocked request unexpectedly committed",
        )
        self.crash_journal.record("original_absent")
        self.crash_journal.record("original_recovery_requested", replica=1)
        status, committed = self.http(
            self.bases[1], "POST", self.route + "/commands", token, request
        )
        require(
            status == 200 and committed["replayed"] is False,
            "Original request did not recover on surviving API",
        )
        self.retain(request, committed["result"], token)
        self.crash_journal.record("original_committed", result=committed["result"])
        if getattr(self.args, "serial_units", 0):
            self.serial_recovery_requests.append(request["request_id"])
        restart_started = time.monotonic()
        self.docker("start", self.args.api[0])
        # Docker may allocate a different random loopback port on container start.
        self.refresh_bindings()
        self.ready()
        restart_to_readiness = time.monotonic() - restart_started
        self.replay(request, token, self.bases[0])
        self.crash_journal.record("exact_replay_verified", replica=0)
        self.crash_journal.record("completed")
        self.phase_evidence["api_sigkill"] = {
            "crash_journal": self.crash_journal.manifest(),
            **self.crash_barrier_evidence(),
            **({"all_table_fingerprints_unchanged": before} if before is not None else {}),
            "database_blocking_pid_barrier": True,
            "exit_code": 137,
            "surviving_replica_ready": True,
            "uncommitted_original_absent": True,
            "same_original_request_recovered_other_replica": True,
            "restarted_replica_exact_replay": True,
            "local_restart_to_readiness_seconds": round(restart_to_readiness, 3),
            "interrupted_response": interrupted,
        }

    def crash_postgres(self):
        print(
            "Crashing PostgreSQL with a blocked HTTP write; both replicas must fail readiness before recovery.",
            flush=True,
        )
        token = self.operators[1][1]
        request = self.recovery_request(True, token)
        self.crash_journal = CrashJournal(self.root, "postgres_sigkill", self.args.run,
                                          self.credentials[token]["actor"], request)
        before = self.fingerprints()
        self.crash_journal.record("baseline_captured", fingerprints=before)
        application, locker = self.locker(request, token)
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            self.crash_journal.record("original_send_requested", replica=1)
            pending = pool.submit(
                self.http,
                self.bases[1],
                "POST",
                self.route + "/commands",
                token,
                request,
                20,
            )
            self.wait_blocked(application)
            self.owned(self.args.postgres)
            self.crash_journal.record("fault_requested", container=self.args.postgres, signal="KILL")
            self.docker("kill", "--signal=KILL", self.args.postgres)
            interrupted = self.consume_future(pending)
            self.crash_journal.record("interrupted_response_observed", response=interrupted)
            locker.communicate(timeout=10)
        require(
            self.owned(self.args.postgres)["State"]["ExitCode"] == 137,
            "PostgreSQL was not abruptly killed",
        )
        self.crash_journal.record("fault_verified", exit_code=137)
        readiness = []
        liveness = []
        for base in self.bases:
            self.wait(
                lambda: self.http(base, "GET", "/ready", timeout=7)[0] == 503,
                "readiness failure while database is stopped",
                20,
            )
            readiness.append(503)
            status, _ = self.http(base, "GET", "/health")
            require(
                status == 200, "API process liveness should survive database outage"
            )
            liveness.append(status)
        restart_started = time.monotonic()
        self.docker("start", self.args.postgres)
        self.ready()
        restart_to_readiness = time.monotonic() - restart_started
        after = self.fingerprints()
        require(
            before == after,
            "Previously committed table data changed across database crash recovery",
        )
        self.crash_journal.record("rollback_verified")
        self.remove_serial_barrier()
        status, receipt = self.http(
            self.bases[0],
            "GET",
            self.route + "/commands/" + request["request_id"],
            token,
        )
        require(
            status == 200 and receipt == {"found": False},
            "Interrupted database transaction unexpectedly committed",
        )
        self.crash_journal.record("original_absent")
        self.crash_journal.record("original_recovery_requested", replica=0)
        status, committed = self.http(
            self.bases[0], "POST", self.route + "/commands", token, request
        )
        require(
            status == 200 and committed["replayed"] is False,
            "Original interrupted request could not recover after database restart",
        )
        self.retain(request, committed["result"], token)
        self.crash_journal.record("original_committed", result=committed["result"])
        if getattr(self.args, "serial_units", 0):
            self.serial_recovery_requests.append(request["request_id"])
        self.replay(request, token, self.bases[1])
        self.crash_journal.record("exact_replay_verified", replica=1)
        logged = subprocess.run(
            ["docker", "logs", self.args.postgres],
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        )
        log = logged.stdout + logged.stderr
        recovery_observed = (
            "database system was interrupted" in log
            or "database system was not properly shut down" in log
        )
        require(
            recovery_observed,
            "PostgreSQL did not report actual unclean-shutdown recovery",
        )
        self.crash_journal.record("completed")
        self.phase_evidence["postgres_sigkill"] = {
            "crash_journal": self.crash_journal.manifest(),
            **self.crash_barrier_evidence(),
            "database_blocking_pid_barrier": True,
            "exit_code": 137,
            "named_volume_preserved": True,
            "readiness_during_outage": readiness,
            "liveness_during_outage": liveness,
            "local_database_restart_to_both_api_readiness_seconds": round(
                restart_to_readiness, 3
            ),
            "both_replicas_recovered_without_api_restart": True,
            "all_table_fingerprints_unchanged": before,
            "unclean_shutdown_recovery_observed": True,
            "uncommitted_original_absent": True,
            "same_original_request_committed_once_then_replayed": True,
            "interrupted_response": interrupted,
        }

    def serial_target(self, move_number):
        if (getattr(self.args, "serial_units", 0)
                and getattr(self.args, "serial_pattern", "fixed") == "rotating"):
            offset = (move_number // 2) % (self.args.lots * self.args.serial_units)
            return divmod(offset, self.args.serial_units)
        return 0, 0

    def move_request(self, index, move_number, reviewed):
        lot, unit = self.serial_target(move_number)
        command = {
            "type": "move_stock",
            "sku": self.sku(index),
            "lot": self.lot(lot),
            "from": self.bin(index) if move_number % 2 == 0 else self.bin(index + 1),
            "to": self.bin(index + 1) if move_number % 2 == 0 else self.bin(index),
            "quantity": 1,
        }
        request = {
            "request_id": str(uuid.uuid4()),
            "expected_version": reviewed["version"],
            "command": command,
        }
        if self.args.review_mode == "resources":
            parts = [
                ["stock", command["sku"], command["lot"], command["from"]],
                ["stock", command["sku"], command["lot"], command["to"]],
                ["product", command["sku"]],
                ["location", command["from"]], ["location", command["to"]],
            ]
            keys = {json.dumps(k, separators=(",", ":"), ensure_ascii=False) for k in parts}
            request["resource_versions"] = {
                k: reviewed["resource_versions"].get(k, 0) for k in sorted(keys)
            }
        if getattr(self.args, "serial_units", 0):
            request["serial_inputs"] = [{"target": [], "sku": self.sku(index),
                                         "serials": [self.serial_id(index, lot, unit)]}]
        return request

    def serial_visibility(self, index, move_number, base, token):
        if not getattr(self.args, "serial_units", 0):
            return None
        started = time.monotonic()
        lot, unit = self.serial_target(move_number)
        sku, label = self.sku(index), self.serial_id(index, lot, unit)
        query = urllib.parse.urlencode({"sku": sku, "serial": label, "limit": 1})
        status, page = self.http(base, "GET", "/v1/serials?" + query, token)
        destination = self.bin(index + 1) if move_number % 2 == 0 else self.bin(index)
        require(status == 200 and page.get("page_source_ok") is True
                and page.get("total_records") == 1 and page.get("next_cursor") is None
                and len(page.get("items", [])) == 1,
                "Post-commit literal serial source review failed")
        item = page["items"][0]
        require(item.get("sku") == sku and item.get("serial") == label
                and item.get("lot") == self.lot(lot) and item.get("source_ok") is True
                and item.get("place") == {"status": "on_hand", "warehouse_id": "wh-main", "location": destination},
                "Post-commit serial identity or exact custody is invisible")
        return (time.monotonic() - started) * 1000

    def reviewed_move(self, index, move_number, reviewed, started, refresh_after_save, stop=None):
        _actor, token = self.operators[index]
        base = self.bases[index % 2]
        conflicts = 0
        attempts = []
        snapshots = []
        while True:
            if stop is not None and stop.is_set():
                return None  # No submission, or the preceding attempt definitively failed.
            request = self.move_request(index, move_number, reviewed)
            attempted = time.monotonic()
            status, data = self.http(base, "POST", self.route + "/commands", token, request)
            attempts.append((time.monotonic() - attempted) * 1000)
            if status == 409:
                require(
                    data.get("command_outcome", {}).get("reason") in ("stale_version", "changed_resources"),
                    "Unexpected request-key conflict",
                )
                conflicts += 1
                require(conflicts < 100, "Writer starvation exceeded bounded qualification retries")
                reviewed, refresh_latency = self.snapshot_review(self.bases[1 - index % 2], token)
                snapshots.append(refresh_latency)
                continue  # Only a definitive rejection permits a reviewed new key.
            require(
                status == 200 and data["replayed"] is False,
                f"Sustained write failed: {status} {data}",
            )
            self.retain(request, data["result"], token)
            if refresh_after_save:
                reviewed, refresh_latency = self.snapshot_review(base, token)
                snapshots.append(refresh_latency)
                require(
                    reviewed["version"] >= data["result"]["version"],
                    "Post-commit snapshot predates the confirmed command",
                )
                # The other operators own different SKUs. Their activity cannot
                # hide this client's exact source/destination custody change.
                stock = {(s["sku"], s["lot"], s["location"]): s["on_hand"] for s in reviewed["stocks"]}
                require(
                    stock.get((self.sku(index), request["command"]["lot"], self.bin(index)), 0)
                    == self.opening_quantity() - (move_number + 1) % 2
                    and stock.get((self.sku(index), request["command"]["lot"], self.bin(index + 1)), 0)
                    == (move_number + 1) % 2,
                    "Post-commit snapshot did not expose the client's physical movement",
                )
            serial_latency = self.serial_visibility(index, move_number, base, token) if refresh_after_save else None
            latency = (time.monotonic() - started) * 1000
            replay_started = time.monotonic()
            replay_status, replay = self.http(
                self.bases[1 - index % 2], "POST", self.route + "/commands", token, request,
            )
            require(
                replay_status == 200 and replay["replayed"] is True and replay["result"] == data["result"],
                "Cross-replica replay changed result",
            )
            return {
                "reviewed": reviewed,
                "conflicts": conflicts,
                "latency": latency,
                "attempts": attempts,
                "snapshots": snapshots,
                "replay_latency": (time.monotonic() - replay_started) * 1000,
                "serial_latency": serial_latency,
                "request_id": request["request_id"],
                "serial_label": request.get("serial_inputs", [{}])[0].get("serials", [None])[0],
                "lot": request["command"]["lot"],
                "action_started_monotonic": started,
                "cycle_ended_monotonic": time.monotonic(),
            }

    def sustained(self):
        independent = self.args.traffic_model == "independent-operators"
        print(
            f"Starting {self.args.seconds}s sustained HTTP qualification with {len(self.operators)} distinct operators, two replicas and {self.args.traffic_model} traffic.",
            flush=True,
        )
        started = time.monotonic()
        self.traffic_started_monotonic = started
        deadline = started + self.args.seconds
        rounds = 0
        moves = [0] * len(self.operators)
        results = [
            {"actor": actor, "commits": 0, "stale_conflicts": 0, "exact_replays": 0,
             "initial_snapshot_reads": 0, "post_commit_refreshes": 0,
             "latencies": [], "attempt_latencies": [], "snapshot_latencies": [], "replay_latencies": [], "serial_latencies": [], "serial_labels": set()}
            for actor, _ in self.operators
        ]
        last_update = started
        stop = threading.Event()

        def record(index, sample):
            result = results[index]
            moves[index] += 1
            result["commits"] += 1
            result["exact_replays"] += 1
            result["stale_conflicts"] += sample["conflicts"]
            result["post_commit_refreshes"] += int(independent)
            result["latencies"].append(sample["latency"])
            result["attempt_latencies"].extend(sample["attempts"])
            result["snapshot_latencies"].extend(sample["snapshots"])
            result["replay_latencies"].append(sample["replay_latency"])
            if sample["serial_latency"] is not None:
                result["serial_latencies"].append(sample["serial_latency"])
                result["serial_labels"].add(sample["serial_label"])

        def progress():
            print(
                f"{time.monotonic() - started:.1f}s: {sum(moves)} committed moves, {sum(r['stale_conflicts'] for r in results)} definite stale conflicts, same number of exact replays.",
                flush=True,
            )

        def client(index):
            try:
                _, token = self.operators[index]
                reviewed, latency = self.snapshot_review(self.bases[index % 2], token)
                results[index]["snapshot_latencies"].append(latency)
                results[index]["initial_snapshot_reads"] += 1
                while time.monotonic() < deadline and not stop.is_set():
                    sample = self.reviewed_move(index, moves[index], reviewed, time.monotonic(), True, stop)
                    if sample is None:
                        break
                    record(index, sample)
                    reviewed = sample["reviewed"]
            except BaseException:
                stop.set()
                raise

        round_snapshot_latencies = []
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(self.operators)) as pool:
            if independent:
                pending = {pool.submit(client, i) for i in range(len(self.operators))}
                try:
                    while pending:
                        done, pending = concurrent.futures.wait(
                            pending, timeout=10, return_when=concurrent.futures.FIRST_EXCEPTION,
                        )
                        for future in done:
                            future.result()  # A failed client cannot become a passing report.
                        if time.monotonic() - last_update >= 10:
                            progress()
                            last_update = time.monotonic()
                finally:
                    stop.set()  # Interrupts must not wait for the full traffic interval.
            else:
                while time.monotonic() < deadline:
                    round_started = time.monotonic()
                    reviewed, latency = self.snapshot_review()
                    round_snapshot_latencies.append(latency)
                    futures = [pool.submit(self.reviewed_move, i, moves[i], reviewed, round_started, False)
                               for i in range(len(self.operators))]
                    for i, future in enumerate(futures):
                        record(i, future.result())
                    rounds += 1
                    if time.monotonic() - last_update >= 10:
                        progress()
                        last_update = time.monotonic()
        elapsed = time.monotonic() - started
        require(elapsed >= self.args.seconds and all(moves), "Every operator must complete work across the full interval")
        conflicts = sum(r["stale_conflicts"] for r in results)
        if self.args.review_mode == "resources":
            require(conflicts == 0, "Independent resource reviews produced spurious conflicts")
        elif not independent:
            require(conflicts >= rounds * (len(self.operators) - 1),
                    "Legacy same-version race did not produce required stale protection")
        latencies = [v for r in results for v in r["latencies"]]
        attempts = [v for r in results for v in r["attempt_latencies"]]
        snapshots = round_snapshot_latencies + [v for r in results for v in r["snapshot_latencies"]]
        replays = [v for r in results for v in r["replay_latencies"]]
        require(len(latencies) == len(replays) == sum(moves), "Incomplete client cycle timing evidence")
        if independent:
            require(len(snapshots) == len(self.operators) + sum(moves) + conflicts,
                    "Every independent client needs its own initial review and post-commit refresh")
        self.moves = moves
        self.phase_evidence["sustained_http"] = {
            "requested_seconds": self.args.seconds,
            "review_mode": self.args.review_mode,
            "traffic_model": self.args.traffic_model,
            "client_schedule": "independent continuous client loops without think time or an operator barrier" if independent else "one full snapshot shared by all clients at each concurrent round barrier",
            "serial_units_per_bin_lot": getattr(self.args, "serial_units", 0),
            "serial_pattern": getattr(self.args, "serial_pattern", "fixed") if getattr(self.args, "serial_units", 0) else None,
            "serial_distinct_labels_per_operator": [len(r["serial_labels"]) for r in results] if getattr(self.args, "serial_units", 0) else [],
            "serial_labels_available_per_operator": (
                self.args.lots * self.args.serial_units if getattr(self.args, "serial_pattern", "fixed") == "rotating" else 1
            ) if getattr(self.args, "serial_units", 0) else 0,
            "serial_current_source_reads": sum(len(r["serial_latencies"]) for r in results),
            "serial_current_source_latency": latency_summary([v for r in results for v in r["serial_latencies"]]),
            "measurement_limit": "Authenticated loopback HTTP and JSON processing; Python clients open HTTP connections per request; browser rendering, human timing and physical scanner input are excluded",
            "elapsed_seconds": round(elapsed, 3),
            "replicas": 2,
            "distinct_operator_identities": len(self.operators),
            "rounds": None if independent else rounds,
            "committed_moves": sum(moves),
            "exact_cross_replica_replays": sum(r["exact_replays"] for r in results),
            "definite_stale_conflicts": conflicts,
            "unexpected_errors": 0,
            "committed_moves_per_second": round(sum(moves) / elapsed, 3),
            "action_latency": latency_summary(latencies),
            "action_latency_scope": (
                "command submission from the client's cached review through committed write and its own validated post-commit full warehouse refresh and, when enabled, exact literal serial source/custody review; initial review and subsequent exact replay excluded"
                if independent else
                "shared round snapshot through committed physical write, including stale conflicts and full snapshot refreshes; subsequent exact replay excluded"
            ),
            "throughput_scope": "all completed client cycles, including initial reviews, physical commands, post-commit refreshes when enabled, conflicts and exact cross-replica replays; replay is not another physical move",
            "http_write_attempt_latency": latency_summary(attempts),
            "frozen_replay_latency": latency_summary(replays),
            "operational_snapshot_latency": latency_summary(snapshots),
            "shared_round_snapshot_reads": len(round_snapshot_latencies),
            "independent_post_commit_stock_visibility_verified": independent,
            "snapshot_coverage": "exact configured catalog SKU set and every seeded order ID/state, with completed history permitted",
            "operators": [
                {**{k: v for k, v in r.items() if not isinstance(v, (list, set))},
                 "snapshot_reads": len(r["snapshot_latencies"])}
                for r in results
            ],
        }

        policy = traffic_policy.evaluate(self.phase_evidence["sustained_http"], self.args)
        self.phase_evidence["sustained_http"]["acceptance"] = policy
        require(policy["ok"], "; ".join(policy["failures"]))

    def independent_proof(self):
        print(
            "Rebuilding physical custody, reservations, exact receipt lines and atomic audit evidence independently from PostgreSQL.",
            flush=True,
        )
        stock_rows = self.table("wms_stock")
        movements = self.table("wms_movements")
        allocations = self.table("wms_allocations")
        receipt_lines = self.table("wms_receipt_lines")
        commands = self.table("wms_commands")
        events = self.table("wms_events")
        activity = self.table("wms_activity")
        outbox = self.table("wms_outbox")
        expected_commands = self.expected_setup_commands() + 13 + sum(self.moves)
        expected_movements = self.args.products * self.args.lots + 9 + sum(self.moves)
        require(
            len(commands) == expected_commands,
            "Total command count differs from independent seed/recovery/physical-action model",
        )
        require(
            len(movements) == expected_movements,
            "Movement count differs from independent opening/receipt/pick/handoff/recovered-action model",
        )
        key = lambda row, location=None: (
            row["sku"],
            row["lot"],
            row["location"] if location is None else location,
        )
        physical = collections.Counter()
        received = collections.Counter()
        for row in movements:
            require(
                row["tenant"] == self.tenant,
                "Cross-tenant movement leaked into evidence",
            )
            if row["from_location"] is not None:
                physical[key(row, row["from_location"])] -= row["quantity"]
            if row["to_location"] is not None:
                physical[key(row, row["to_location"])] += row["quantity"]
            if row["kind"] == "receipt":
                received[(row["receipt_id"], row["receipt_line_id"])] += row["quantity"]
        reserved = collections.Counter()
        for row in allocations:
            reserved[key(row)] += row["quantity"] - row["picked"] - row["released"]
            reserved[key(row, "STAGE-01")] += row["picked"] - row["shipped"]
        actual = {key(row): (row["on_hand"], row["reserved"]) for row in stock_rows}
        for identity in set(actual) | set(physical) | set(reserved):
            require(
                actual.get(identity, (0, 0))
                == (physical[identity], reserved[identity]),
                f"Independent physical/reservation mismatch: {identity}",
            )
        for row in receipt_lines:
            require(
                row["received"] == received[(row["receipt_id"], row["id"])],
                "Exact receipt line lacks matching append-only movements",
            )
        require(
            {
                row["id"]: row["received"]
                for row in receipt_lines
                if row["receipt_id"] == self.receipt_id
            }
            == {"same-sku-first": 7, "same-sku-second": 11},
            "Repeated SKU receipt lines drifted",
        )
        require(
            sum(row["on_hand"] for row in stock_rows if row["sku"] == "PROBE") == 13,
            "Independent receive18 minus shipped5 is not13",
        )
        expected_reserved = 3 + (self.args.open_orders + 1) // 2
        expected_physical = self.args.products * self.args.lots * self.opening_quantity() + 13
        require(
            sum(row["reserved"] for row in stock_rows) == expected_reserved,
            "Outstanding operational/recovery reservations changed",
        )
        require(
            sum(row["on_hand"] for row in stock_rows) == expected_physical,
            "Total opening plus received minus shipped custody changed",
        )
        for i, count in enumerate(self.moves):
            lot, _ = self.serial_target(count - 1)
            require(
                actual[(self.sku(i), self.lot(lot), self.bin(i))][0]
                == self.opening_quantity() - count % 2,
                "Per-operator source bin conservation failed",
            )
            require(
                actual.get((self.sku(i), self.lot(lot), self.bin(i + 1)), (0, 0))[0]
                == count % 2,
                "Per-operator destination bin conservation failed",
            )
        orders = self.table("wms_orders")
        seeded_order_ids = set(self.seeded_orders)
        seeded_orders = {
            row["id"]: row for row in orders if row["id"] in seeded_order_ids
        }
        require(
            len(seeded_orders) == self.args.open_orders,
            "Operational work disappeared during qualification",
        )
        for i, order_id in enumerate(self.seeded_orders):
            require(
                seeded_orders[order_id]["status"]
                == ("allocated" if i % 2 == 0 else "pending"),
                "Operational order state changed during stock transfers",
            )
        require(
            len(commands) == len(self.accepted) == len(events) == len(activity),
            "Commands/events/activity did not commit one-to-one",
        )
        by_request = {row["request_id"]: row for row in commands}
        event_by_request = {row["payload"]["request_id"]: row for row in events}
        require(
            set(by_request) == set(event_by_request) == set(self.accepted),
            "Atomic receipts/event request identities differ",
        )
        for request_id, expected in self.accepted.items():
            command = by_request[request_id]
            event = event_by_request[request_id]
            require(
                command["actor"] == event["payload"]["actor"] == expected["actor"],
                "Operator ownership changed",
            )
            require(
                command["result"] == event["payload"]["result"] == expected["result"],
                "Atomic command/event result changed",
            )
            require(
                event["payload"]["command"] == expected["request"]["command"],
                "Committed event command changed",
            )
        versions = sorted(row["version"] for row in events)
        require(
            versions == list(range(1, len(commands) + 1))
            and self.version() == len(commands),
            "Warehouse version/event sequence has a gap or duplicate",
        )
        destination_counts = collections.Counter(
            (row["event_id"], row["destination"]) for row in outbox
        )
        for event in events:
            require(
                destination_counts[(event["id"], "oms")] == 1,
                "OMS outbox not atomically retained exactly once",
            )
            expected_tms = int(event["payload"]["command"]["type"] in ("pack", "ship"))
            require(
                destination_counts[(event["id"], "tms")] == expected_tms,
                "TMS outbox identity mismatch",
            )
        require(len(outbox) == len(commands) + 2, "Unexpected/missing outbox records")
        require(
            all(row["status"] == "pending" and row["attempts"] == 0 for row in outbox),
            "Unconfigured external delivery unexpectedly ran",
        )
        require(
            self.table("wms_delivery_attempts") == [],
            "Unconfigured external receiver acquired delivery evidence",
        )
        status, outsider = self.http(self.bases[1], "GET", self.route, self.outsider)
        require(
            status == 200
            and outsider["version"] == 0
            and outsider["products"] == []
            and outsider["stocks"] == [],
            "Cross-tenant isolation failed",
        )
        for i, (actor, token) in enumerate(self.operators):
            own = next(v for v in self.accepted.values() if v["actor"] == actor)
            require(
                self.http(
                    self.bases[1 - i % 2],
                    "POST",
                    self.route + "/commands",
                    self.operators[(i + 1) % len(self.operators)][1],
                    own["request"],
                )[0]
                == 409,
                "Another actor replayed retained ownership",
            )
        reconciliations = []
        for base in self.bases:
            proof, run = self.reconciliation_run(base)
            require(
                proof["ok"] is True and proof["version"] == len(commands),
                "API full ledger reconciliation failed",
            )
            reconciliations.append(
                {
                    "ok": proof["ok"],
                    "version": proof["version"],
                    "method": proof["method"],
                    "run_id": run["id"],
                    "run_started_at": run["started_at"],
                    "run_finished_at": run["finished_at"],
                }
            )
        literal_proof = None
        if getattr(self.args, "serial_units", 0):
            tables = {name: self.table(name) for name in (
                "wms_serial_units", "wms_serial_steps", "wms_serial_observations",
                "wms_serial_heads", "wms_serial_batches")}
            tables["wms_events"] = events
            literal_proof = serial_proof(tables, self.accepted, self.args.products,
                                         self.args.lots, self.args.serial_units, self.moves, self.args.bins,
                                         self.serial_recovery_requests,
                                         pattern=getattr(self.args, "serial_pattern", "fixed"))
            require(literal_proof["distinct_timed_labels"] == sum(
                self.phase_evidence["sustained_http"]["serial_distinct_labels_per_operator"]),
                "Observed serial coverage differs from independent literal custody proof")
        return {
            "serial_identity_proof": literal_proof,
            "physical_recomputed_from_append_only_movements": True,
            "reserved_recomputed_from_allocation_lines": True,
            "receipt_quantities_recomputed_per_exact_line": True,
            "expected_probe_on_hand": 13,
            "expected_total_on_hand": expected_physical,
            "expected_reserved": expected_reserved,
            "all_seeded_open_order_states_retained": True,
            "per_operator_bin_totals_match_committed_move_parity": True,
            "commands": len(commands),
            "events": len(events),
            "activity": len(activity),
            "movements": len(movements),
            "outbox": len(outbox),
            "independent_execution_counts": {
                "expected": {
                    "setup_commands": self.expected_setup_commands(),
                    "recovery_commands": 13,
                    "sustained_physical_commands": sum(self.moves),
                    "commands": expected_commands,
                    "events": expected_commands,
                    "activity": expected_commands,
                    "movements": expected_movements,
                    "outbox": expected_commands + 2,
                },
                "observed": {
                    "setup_commands": self.phase_evidence["seed"][
                        "observed_setup_commands"
                    ],
                    "recovery_commands": len(commands)
                    - self.phase_evidence["seed"]["observed_setup_commands"]
                    - sum(self.moves),
                    "sustained_physical_commands": sum(self.moves),
                    "commands": len(commands),
                    "events": len(events),
                    "activity": len(activity),
                    "movements": len(movements),
                    "outbox": len(outbox),
                },
                "all_match": True,
            },
            "contiguous_versions_and_atomic_actor_result_evidence": True,
            "cross_tenant_isolation": True,
            "cross_actor_replay_rejected": True,
            "unconfigured_external_delivery_attempts": 0,
            "replica_reconciliations": reconciliations,
            "final_table_fingerprints": self.fingerprints(),
        }

    def report(self, proof):
        image = json.loads((self.root / "image-inspect.json").read_text())[0]
        replicas = [self.owned(container) for container in self.args.api]
        require(
            all(row["Image"] == image["Id"] for row in replicas),
            "Replicas differ from immutable qualified image",
        )
        for replica in replicas:
            require(
                replica["Config"]["User"] == "10001"
                and replica["HostConfig"]["ReadonlyRootfs"] is True,
                "Replica runtime hardening drifted",
            )
            require(
                "ALL" in replica["HostConfig"]["CapDrop"]
                and "no-new-privileges:true" in replica["HostConfig"]["SecurityOpt"],
                "Replica capability hardening drifted",
            )
        pg = self.owned(self.args.postgres)
        volumes = [
            mount["Name"]
            for mount in pg["Mounts"]
            if mount["Type"] == "volume"
            and mount["Destination"] == "/var/lib/postgresql/data"
        ]
        require(
            len(volumes) == 1,
            "PostgreSQL data does not use exactly one persistent named volume",
        )
        volume = json.loads(self.docker("volume", "inspect", volumes[0]))[0]
        require(
            volume["Labels"].get(self.label) == self.args.run,
            "Database volume ownership label mismatch",
        )
        migrations = self.table("wms_schema_migrations")
        for migration in migrations:
            file = pathlib.Path("migrations") / (migration["name"] + ".sql")
            require(
                hashlib.sha256(file.read_bytes()).hexdigest() == migration["digest"],
                "Running database migration differs from checked source",
            )
        require(
            self.qualifier_hashes == self.script_hashes(),
            "Qualifier scripts changed during execution; rerun on stable files",
        )
        report = {
            "ok": True,
            "synthetic": True,
            "qualified_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "qualification": "two-replica sustained HTTP and crash recovery",
            "image_id": image["Id"],
            "image_source_inputs_sha256": image["Config"]["Labels"].get(
                "org.stateset.wms.source-sha256"
            ),
            "image_release_revision": image["Config"]["Labels"].get(
                "org.opencontainers.image.revision"
            ),
            "qualifier_sha256": self.qualifier_hashes,
            "postgres_image_id": pg["Image"],
            "postgres_version": self.sql("SHOW server_version;"),
            "runtime": {
                "api_replicas": 2,
                "uid": 10001,
                "read_only_rootfs": True,
                "capabilities_dropped": "ALL",
                "no_new_privileges": True,
                "postgres_named_volume_survived_crash": True,
                "random_loopback_api_ports": True,
                "native_and_webhook_connections": 0,
            },
            "elapsed_total_seconds": round(time.time() - self.started, 3),
            "phases": self.phase_evidence,
            "independent_proof": proof,
            "migrations": [
                {"name": row["name"], "sha256": row["digest"]} for row in migrations
            ],
            "qualification_limit": "Local synthetic single-host and single-PostgreSQL-primary rehearsal. API replica survival and PostgreSQL crash recovery were tested; database failover, host loss, deployed latency, native receivers and physical facility acceptance were not.",
        }
        (self.root / "resilience.json").write_text(json.dumps(report, indent=2) + "\n")
        print(
            json.dumps(
                {
                    "ok": True,
                    "image_id": image["Id"],
                    "sustained_http": self.phase_evidence["sustained_http"],
                    "report": str(self.root / "resilience.json"),
                },
                indent=2,
            ),
            flush=True,
        )

    def run(self):
        self.seed()
        self.lose_success()
        self.lifecycle()
        self.crash_api()
        self.crash_postgres()
        # Keep an exact synthetic read fixture outside the measured interval.
        # It permits serialization/validation comparisons without credentials,
        # database setup, or a second competing load generator.
        snapshot, _ = self.snapshot_review()
        fixture = json.dumps(snapshot, sort_keys=True, separators=(",", ":")).encode()
        (self.root / "snapshot-fixture.json").write_bytes(fixture)
        self.phase_evidence["snapshot_fixture"] = {
            "sha256": hashlib.sha256(fixture).hexdigest(),
            "bytes": len(fixture),
            "version": snapshot["version"],
            "outside_sustained_interval": True,
        }
        self.sustained()
        self.report(self.independent_proof())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", required=True)
    parser.add_argument("--run", required=True)
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--api", action="append", required=True)
    parser.add_argument("--seconds", type=int, default=60)
    parser.add_argument("--review-mode", choices=("resources", "warehouse"), default="resources")
    parser.add_argument("--traffic-model", choices=("shared-snapshot", "independent-operators"), default="independent-operators")
    parser.add_argument("--products", type=int, default=100)
    parser.add_argument("--open-orders", type=int, default=100)
    parser.add_argument("--lots", type=int, default=2)
    parser.add_argument("--bins", type=int, default=20)
    parser.add_argument("--serial-units", type=int, default=0, help="0 retains untracked traffic; otherwise literal each IDs per opening bin/lot")
    parser.add_argument("--serial-pattern", choices=("fixed", "rotating"), default="rotating",
                        help="Rotate each out-and-back pair through every label and lot per operator")
    traffic_policy.arguments(parser)
    args = parser.parse_args()
    require(
        re.fullmatch(r"[0-9]+-[0-9a-f]{8}", args.run), "Invalid ownership run identity"
    )
    require(
        len(args.api) == 2 and len(set(args.api)) == 2,
        "Exactly two distinct API containers required",
    )
    require(
        60 <= args.seconds <= 3600,
        "Qualification interval must be between 60 and 3600 seconds",
    )
    require(
        2 <= args.products <= 2000, "Operational products must be between 2 and 2000"
    )
    require(0 <= args.open_orders <= 5000, "Open orders must be between 0 and 5000")
    require(1 <= args.lots <= 10, "Lots must be between 1 and 10")
    require(2 <= args.bins <= 1000, "Storage bins must be between 2 and 1000")
    require(args.serial_units == 0 or 2 <= args.serial_units <= 256,
            "Serial units must be zero or2..256 per bin/lot")
    require(not args.serial_units or (args.traffic_model == "independent-operators"
            and args.serial_units > (args.open_orders + args.products - 1) // args.products),
            "Serial traffic needs independent clients and opening custody above active order allocations")
    credentials = json.loads(
        (pathlib.Path(args.artifacts) / "credentials.json").read_text()
    )
    require(
        args.products
        >= sum(identity["role"] == "operator" for identity in credentials.values()),
        "Products must cover every operator identity",
    )
    Probe(args).run()


if __name__ == "__main__":
    main()

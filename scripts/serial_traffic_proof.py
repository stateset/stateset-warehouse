"""Independent literal serial custody proof for synthetic HTTP traffic."""

import collections


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def serial_proof(tables, accepted, products, lots, units, moves, bins, recovery_requests=(), pattern="fixed"):
    """Rebuild exact literal custody independently of stored guard booleans."""
    events = {e["id"]: e for e in tables["wms_events"]}
    require(pattern in ("fixed", "rotating"), "Unknown serial traffic pattern")
    by_request = {e["payload"]["request_id"]: e for e in events.values()}
    require(
        len(by_request) == len(events) == len(accepted),
        "Event/request population differs",
    )
    # Two deliberately interrupted original requests recover outside the timed
    # traffic interval. Their exact static label goes out and back, independent
    # of the first label moved by each sustained operator.
    require(len(recovery_requests) in (0, 2) and len(set(recovery_requests)) == len(recovery_requests),
            "Serial recovery requires two distinct original requests")
    recovery_key = (f"LOAD-{products - 1:05}", f"Load-{products - 1:05}-Lot-000-Unit-{units - 1:05}")
    recovery_events = []
    for returning, request_id in enumerate(recovery_requests):
        saved = accepted[request_id]["request"]
        expected = {"type": "move_stock", "sku": recovery_key[0], "lot": "LOT-000",
                    "from": f"BIN-{(products - 1 + returning) % bins:05}",
                    "to": f"BIN-{(products - returning) % bins:05}", "quantity": 1}
        require(saved["command"] == expected
                and saved["serial_inputs"] == [{"target": [], "sku": recovery_key[0], "serials": [recovery_key[1]]}]
                and by_request[request_id]["payload"]["command"] == expected,
                "Serial recovery changed its literal label or original physical move")
        recovery_events.append(by_request[request_id]["id"])
    by_unit = collections.defaultdict(list)
    for step in tables["wms_serial_steps"]:
        by_unit[(step["sku"], step["serial"])].append(step)
    actual = {(u["sku"], u["serial"]): u for u in tables["wms_serial_units"]}
    require(
        len(actual) == len(tables["wms_serial_units"]) == products * lots * units,
        "Literal identity population changed",
    )
    expected_steps = products * lots * units + sum(moves) + len(recovery_requests)
    require(
        len(tables["wms_serial_steps"]) == expected_steps,
        "Serial step population changed",
    )
    expected_keys = set()
    for i in range(products):
        sku = f"LOAD-{i:05}"
        for lot in range(lots):
            for n in range(units):
                key = (sku, f"Load-{i:05}-Lot-{lot:03}-Unit-{n:05}")
                expected_keys.add(key)
                count = 0
                if i < len(moves):
                    if pattern == "fixed":
                        count = moves[i] if lot == 0 and n == 0 else 0
                    else:
                        # Independent closed-form expectation; do not derive
                        # expected custody from the observed steps or scheduler.
                        pairs, remainder = divmod(moves[i], 2)
                        cycles, offset = divmod(pairs, lots * units)
                        position = lot * units + n
                        count = 2 * (cycles + int(position < offset))
                        count += int(bool(remainder) and position == offset)
                if key == recovery_key:
                    count += len(recovery_requests)
                place = {
                    "status": "on_hand",
                    "warehouse_id": "wh-main",
                    "location": f"BIN-{(i + count % 2) % bins:05}",
                }
                row = actual.get(key, {})
                require(
                    row.get("lot") == f"LOT-{lot:03}"
                    and row.get("place") == place
                    and row.get("revision") == count + 1,
                    "Literal identity, immutable lot, revision or custody changed",
                )
                chain = sorted(by_unit[key], key=lambda s: s["revision"])
                if key == recovery_key and recovery_requests:
                    recovered = [s for s in chain if s["event_id"] in recovery_events]
                    require([s["event_id"] for s in recovered] == recovery_events,
                            "Serial recovery steps do not belong to the two original requests")
                    for returning, step in enumerate(recovered):
                        require(step["before_place"] == {
                            "status": "on_hand", "warehouse_id": "wh-main",
                            "location": f"BIN-{(i + returning) % bins:05}"}
                            and step["after_place"] == {
                            "status": "on_hand", "warehouse_id": "wh-main",
                            "location": f"BIN-{(i + 1 - returning) % bins:05}"},
                            "Recovered literal custody does not move out and back")
                require(
                    [s["revision"] for s in chain] == list(range(1, count + 2)),
                    "Serial revisions have a missing or extra predecessor",
                )
                previous = None
                for revision, step in enumerate(chain, 1):
                    event = events[step["event_id"]]
                    payload = event["payload"]
                    require(
                        step["before_place"] == previous
                        and step["lot"] == f"LOT-{lot:03}"
                        and step["previous_revision"]
                        == (revision - 1 if revision > 1 else None)
                        and step["warehouse_id"] == "wh-main"
                        and step["warehouse_version"] == event["version"]
                        and step["actor"] == payload["actor"],
                        "Serial predecessor, facility, lot or original actor changed",
                    )
                    normalized = {k: v for k, v in step.items() if k != "tenant"}
                    require(
                        payload["serial_steps"][step["ordinal"] - 1] == normalized,
                        "Normalized serial history differs from its original event",
                    )
                    previous = step["after_place"]
                require(
                    previous == place and row["last_event_id"] == chain[-1]["event_id"],
                    "Latest unit does not match its exact original step",
                )
    require(
        set(actual) == set(by_unit) == expected_keys,
        "Unexpected or missing literal identity",
    )
    observations = collections.defaultdict(list)
    for o in tables["wms_serial_observations"]:
        observations[o["event_id"]].append(o)
    for request_id, saved in accepted.items():
        event = by_request[request_id]
        payload = event["payload"]
        inputs = saved["request"].get("serial_inputs", [])
        require(
            payload.get("serial_inputs", []) == inputs,
            "Original serial observations changed",
        )
        observed = sorted(observations[event["id"]], key=lambda o: o["ordinal"])
        require(
            len(observed) == len(inputs), "Missing or extra original observation group"
        )
        for ordinal, (o, expected) in enumerate(zip(observed, inputs), 1):
            require(
                {"target": o["target"], "sku": o["sku"], "serials": o["serials"]}
                == expected
                and o["ordinal"] == ordinal
                and o["warehouse_id"] == "wh-main"
                and o["sku_version"] == payload["serial_heads"][o["sku"]],
                "Original complete label group or global SKU review changed",
            )
    require(
        sum(map(len, observations.values())) == products * lots + sum(moves) + len(recovery_requests),
        "Serial observation population differs from original counts and moves",
    )
    heads = {h["sku"]: h for h in tables["wms_serial_heads"]}
    require(
        set(heads) == {f"LOAD-{i:05}" for i in range(products)},
        "Global serial head population changed",
    )
    batches = collections.defaultdict(list)
    for b in tables["wms_serial_batches"]:
        batches[b["sku"]].append(b)
    for i in range(products):
        sku = f"LOAD-{i:05}"
        version = lots + (moves[i] if i < len(moves) else 0)
        if i == products - 1:
            version += len(recovery_requests)
        chain = sorted(batches[sku], key=lambda b: b["version"])
        require(
            [b["version"] for b in chain] == list(range(1, version + 1))
            and heads[sku]["version"] == version
            and heads[sku]["last_event_id"] == chain[-1]["event_id"],
            "Global serial batch/head sequence changed",
        )
        for b in chain:
            event = events[b["event_id"]]
            require(
                b["warehouse_id"] == event["warehouse_id"] == "wh-main"
                and b["warehouse_version"] == event["version"]
                and event["payload"]["serial_heads"][sku] == b["version"],
                "Original serial batch event binding changed",
            )
    return {
        "ok": True,
        "literal_units": len(actual),
        "steps": expected_steps,
        "observations": sum(map(len, observations.values())),
        "batches": len(tables["wms_serial_batches"]),
        "global_heads": len(heads),
        "timed_moves": sum(moves),
        "serial_pattern": pattern,
        "distinct_timed_labels": sum(
            min((count + 1) // 2, lots * units) if pattern == "rotating" else int(count > 0)
            for count in moves
        ),
        "recovered_moves_outside_timed_interval": len(recovery_requests),
        "recovery_request_ids": list(recovery_requests),
        "all_literal_custody_revisions_original_steps_observations_and_heads_match": True,
    }

"""Predeclared workload acceptance; measurements never choose their own limits."""
import argparse
import math


def positive(value):
    try:
        number = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("Expected a finite positive number") from error
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("Expected a finite positive number")
    return number


def fraction(value):
    number = positive(value)
    if number > 1:
        raise argparse.ArgumentTypeError("Coverage must be greater than zero and at most one")
    return number


def arguments(parser):
    parser.add_argument("--max-action-p95-ms", type=positive)
    parser.add_argument("--min-moves-per-second", type=positive)
    parser.add_argument("--min-serial-coverage", type=fraction, default=1.0,
                        help="Minimum observed fraction of each operator's configured label/lot population")


def evaluate(report, args):
    limits = {name: getattr(args, name, None) for name in
              ("max_action_p95_ms", "min_moves_per_second", "min_serial_coverage")}
    failures = []
    if limits["max_action_p95_ms"] is not None:
        if report["action_latency"]["p95_ms"] > limits["max_action_p95_ms"]:
            failures.append("action p95 exceeds the predeclared limit")
    if limits["min_moves_per_second"] is not None:
        if report["committed_moves_per_second"] < limits["min_moves_per_second"]:
            failures.append("committed throughput is below the predeclared minimum")
    population = report.get("serial_labels_available_per_operator", 0)
    coverage = [n / population for n in report.get("serial_distinct_labels_per_operator", [])] if population else []
    if population and limits["min_serial_coverage"] is not None:
        if (len(coverage) != report["distinct_operator_identities"]
                or any(n < limits["min_serial_coverage"] for n in coverage)):
            failures.append("not every operator exercised the required serial population")
    return {"ok": not failures, "limits": limits, "failures": failures,
            "observed_serial_coverage_per_operator": coverage,
            "scope": "Synthetic workload acceptance; unset latency/throughput limits establish no facility SLO"}

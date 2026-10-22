"""Acceptance limits reject insufficient measured work, including NaN inputs."""
import argparse
from contextlib import redirect_stderr
from copy import deepcopy
import io
from types import SimpleNamespace
import unittest

import traffic_policy as policy


class TrafficPolicyTests(unittest.TestCase):
    def setUp(self):
        self.args = SimpleNamespace(max_action_p95_ms=500, min_moves_per_second=10,
                                    min_serial_coverage=1)
        self.report = {"action_latency": {"p95_ms": 500}, "committed_moves_per_second": 10,
                       "serial_labels_available_per_operator": 32,
                       "serial_distinct_labels_per_operator": [32, 32],
                       "distinct_operator_identities": 2}

    def test_exact_boundaries_pass_and_each_failed_measurement_holds_acceptance(self):
        self.assertTrue(policy.evaluate(self.report, self.args)["ok"])
        for field, value in [("action_latency", {"p95_ms": 500.001}),
                             ("committed_moves_per_second", 9.999),
                             ("serial_distinct_labels_per_operator", [32, 31]),
                             ("serial_distinct_labels_per_operator", [32]),
                             ("serial_distinct_labels_per_operator", [])]:
            with self.subTest(field=field, value=value):
                report = deepcopy(self.report)
                report[field] = value
                result = policy.evaluate(report, self.args)
                self.assertFalse(result["ok"])
                self.assertTrue(result["failures"])
                self.assertEqual(result["limits"]["min_serial_coverage"], 1)

    def test_unset_speed_limits_do_not_claim_a_facility_slo(self):
        result = policy.evaluate(self.report, SimpleNamespace(min_serial_coverage=1))
        self.assertTrue(result["ok"])
        self.assertIsNone(result["limits"]["max_action_p95_ms"])
        self.assertIsNone(result["limits"]["min_moves_per_second"])

    def test_nonserial_profiles_do_not_claim_label_coverage(self):
        self.report.update(serial_labels_available_per_operator=0,
                           serial_distinct_labels_per_operator=[])
        result = policy.evaluate(self.report, self.args)
        self.assertTrue(result["ok"])
        self.assertEqual(result["observed_serial_coverage_per_operator"], [])

    def test_invalid_limits_fail_during_argument_parsing(self):
        parser = argparse.ArgumentParser()
        policy.arguments(parser)
        for flag in ["--max-action-p95-ms", "--min-moves-per-second", "--min-serial-coverage"]:
            for value in ["nan", "inf", "-inf", "0", "-1", "text"]:
                with self.subTest(flag=flag, value=value), redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit):
                        parser.parse_args([flag + "=" + value])
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            parser.parse_args(["--min-serial-coverage=1.01"])


if __name__ == "__main__":
    unittest.main()

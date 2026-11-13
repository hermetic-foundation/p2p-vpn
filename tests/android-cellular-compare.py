import contextlib
import copy
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

path = Path(
    os.environ.get(
        "P2P_VPN_CELLULAR_COMPARE",
        Path(__file__).parent.parent / "scripts/android-cellular-compare.py",
    )
)
spec = importlib.util.spec_from_file_location("compare", path)
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)


def sha(value):
    return hashlib.sha256(str(value).encode()).hexdigest()


def fixture(arm="reference", offset=0):
    context = {
        "app_pid": 1234,
        "runtime_generation": 1,
        "screen_interactive": False,
        "wifi_enabled": False,
        "underlay_kind": "cellular",
        "underlay_validated": True,
        "underlay_generation": sha("underlay"),
        "accounting_mode": "simulated_on_battery",
        "physical_usb_powered": True,
        "power_save": False,
        "device_idle": "INACTIVE",
        "thermal_status": 0,
        "temperature_tenths_c": 280,
        "rsrp_dbm": -85,
        "authorized_peers": 5,
        "payload_sent_packets": 10,
        "payload_received_packets": 12,
    }
    metadata = {
        "schema_version": 1,
        "uid": 10304,
        "arm": arm,
        "settle_seconds": 300,
        "window_seconds": 1800,
        "identity": {key: sha(key) for key in compare.IDENTITIES},
        "probes": {
            key: True
            for key in (
                "inbound_before",
                "outbound_before",
                "inbound_after",
                "outbound_after",
            )
        },
    }
    metadata["identity"]["apk"] = sha(arm)
    reports = []
    for key, elapsed, charge in (
        ("start", 0, 100),
        ("end", 1800000, 150 if arm == "reference" else 102),
    ):
        message = compare.decoder.report_type()()
        stats = message.batterystats
        stats.report_version, stats.parcel_version = 36, 216
        stats.uids.add(uid=10304).power_use_item.computed_power_mah = charge
        battery = stats.system.battery
        for field in compare.decoder.SCOPE_FIELDS:
            setattr(battery, field, 100000 + elapsed + offset)
        battery.start_clock_time_ms = 1700000000000
        battery.start_count = 1
        reports.append(message)
        metadata[key] = {
            "boot_id": "a1234567-89ab-4cde-8f01-234567890abc",
            "before_uptime_ms": 10000000 + elapsed + offset - 5,
            "after_uptime_ms": 10000000 + elapsed + offset + 5,
            "context": copy.deepcopy(context),
        }
    return metadata, reports


def accounting(metadata, reports):
    return compare.report_window(*(r.SerializeToString() for r in reports), metadata)


def rail_rows(start=0, end=4000000000, rate=1000000):
    return [
        {
            "name": name,
            "track_id": str(track),
            "ts": str(ts),
            "value": str(ts * rate / 1e9),
        }
        for track, name in enumerate(compare.RAILS, 1)
        for ts in range(start, end + 1, 1000000000)
    ]


def series():
    windows = []
    for index, arm in enumerate(compare.ORDER):
        metadata, reports = fixture(arm, index * 2400000)
        interval = accounting(metadata, reports)
        energy = 1000 if arm == "reference" else 100
        windows.append(
            {
                "valid": True,
                "metadata": metadata,
                "accounting": interval,
                "functionality_passed": True,
                "trace": {
                    "sha256": sha(index),
                    "rails": [
                        {
                            "name": name,
                            "mean_mw": energy / 1.8,
                            "energy_joules_bounds": [energy - 1, energy + 1],
                        }
                        for name in compare.RAILS
                    ],
                },
            }
        )
    return {
        "schema_version": 1,
        "reference_apk": sha("reference"),
        "candidate_apk": sha("candidate"),
        "window_seconds": 1800,
    }, windows


class AccountingTests(unittest.TestCase):
    def test_full_precision_rate(self):
        metadata, reports = fixture()
        reports[1].batterystats.uids[
            0
        ].power_use_item.computed_power_mah = 150.123456789
        result = accounting(metadata, reports)
        self.assertAlmostEqual(result["app_mah_per_hour"], 100.246913578)
        self.assertEqual(result["elapsed_seconds"], 1800)
        self.assertEqual(result["start_boottime_ns"], 10000000000000)
        self.assertEqual(result["clock_uncertainty_ms"], 15)

    def test_ntp_label_change_is_not_a_reset(self):
        metadata, reports = fixture()
        reports[1].batterystats.system.battery.start_clock_time_ms -= 1200
        self.assertEqual(accounting(metadata, reports)["app_mah_per_hour"], 100)

    def test_zero_delta_is_available(self):
        metadata, reports = fixture()
        reports[1].batterystats.uids[0].power_use_item.computed_power_mah = 100
        self.assertEqual(accounting(metadata, reports)["app_mah_per_hour"], 0)

    def test_missing_attribution_is_not_zero(self):
        metadata, reports = fixture()
        reports[1].batterystats.uids[0].ClearField("power_use_item")
        with self.assertRaisesRegex(compare.InvalidWindow, "attribution_unavailable"):
            accounting(metadata, reports)

    def test_boot_reset_scope_and_bracket_changes(self):
        for mutation in ("boot", "session", "bracket", "start_count", "reversal"):
            metadata, reports = fixture()
            if mutation == "boot":
                metadata["end"]["boot_id"] = "b1234567-89ab-4cde-8f01-234567890abc"
            elif mutation == "session":
                metadata["end"]["before_uptime_ms"] += 10000
                metadata["end"]["after_uptime_ms"] += 10000
            elif mutation == "bracket":
                metadata["start"]["after_uptime_ms"] += 5000
            elif mutation == "start_count":
                reports[1].batterystats.system.battery.start_count += 1
            else:
                reports[1].batterystats.uids[0].power_use_item.computed_power_mah = 99
            with (
                self.subTest(mutation=mutation),
                self.assertRaises(compare.InvalidWindow),
            ):
                accounting(metadata, reports)

    def test_screen_wake_and_missing_battery_accounting(self):
        for field in ("screen_off_realtime_ms", "battery_realtime_ms"):
            metadata, reports = fixture()
            battery = reports[1].batterystats.system.battery
            setattr(battery, field, getattr(battery, field) - 1000)
            battery.screen_off_uptime_ms -= 2000
            battery.battery_uptime_ms -= 2000
            battery.screen_off_realtime_ms = min(
                battery.screen_off_realtime_ms, battery.battery_realtime_ms
            )
            with self.subTest(field=field), self.assertRaises(compare.InvalidWindow):
                accounting(metadata, reports)

    def test_old_ten_minute_diagnostic_is_not_acceptance(self):
        metadata, reports = fixture()
        metadata["window_seconds"] = 600
        with self.assertRaisesRegex(compare.InvalidWindow, "window_duration"):
            accounting(metadata, reports)

    def test_boolean_numbers_are_rejected(self):
        for field in ("schema_version", "uid", "window_seconds"):
            metadata, reports = fixture()
            metadata[field] = True
            with self.subTest(field=field), self.assertRaises(compare.InvalidWindow):
                accounting(metadata, reports)

    def test_energy_overflow_is_rejected(self):
        metadata, reports = fixture()
        reports[1].batterystats.uids[0].power_use_item.computed_power_mah = 1e308
        with self.assertRaisesRegex(compare.InvalidWindow, "energy_overflow"):
            accounting(metadata, reports)


class ContextTests(unittest.TestCase):
    def test_good_context_and_failed_probe_remain_distinct(self):
        metadata, _ = fixture()
        self.assertTrue(compare.validate_context(metadata))
        metadata["probes"]["inbound_after"] = False
        self.assertFalse(compare.validate_context(metadata))

    def test_cellular_screen_power_and_restart_guards(self):
        changes = {
            "screen_interactive": True,
            "wifi_enabled": True,
            "underlay_kind": "wifi",
            "underlay_validated": False,
            "app_pid": 4321,
            "runtime_generation": 2,
            "underlay_generation": sha("changed"),
            "thermal_status": 1,
            "temperature_tenths_c": 350,
            "rsrp_dbm": -110,
            "authorized_peers": 4,
            "accounting_mode": "discharging",
            "device_idle": "IDLE",
            "power_save": True,
            "payload_sent_packets": 11,
            "payload_received_packets": 13,
        }
        for field, value in changes.items():
            metadata, _ = fixture()
            metadata["end"]["context"][field] = value
            with self.subTest(field=field), self.assertRaises(compare.InvalidWindow):
                compare.validate_context(metadata)

    def test_unsettled_missing_identity_and_private_extra_fields(self):
        metadata, _ = fixture()
        metadata["settle_seconds"] = 299
        with self.assertRaises(compare.InvalidWindow):
            compare.validate_context(metadata)
        for field in ("identity", "context", "metadata"):
            metadata, _ = fixture()
            target = (
                metadata["identity"]
                if field == "identity"
                else metadata["start"]["context"]
                if field == "context"
                else metadata
            )
            target["private_extra"] = "do-not-echo"
            with self.subTest(field=field), self.assertRaises(compare.InvalidWindow):
                compare.validate_context(metadata)


class RailTests(unittest.TestCase):
    def test_interpolated_clipped_interval_and_bounds(self):
        result = compare.integrate_rails(rail_rows(), 500000000, 3500000000, 10000000)
        for rail in result:
            self.assertEqual(rail["energy_joules"], 3)
            self.assertAlmostEqual(rail["energy_joules_bounds"][0], 2)
            self.assertAlmostEqual(rail["energy_joules_bounds"][1], 4)
            self.assertEqual(rail["mean_mw"], 1000)

    def test_bursts_between_boundary_samples_cannot_falsely_corroborate(self):
        plan, windows = series()
        for arm, values in (
            ("reference", (0, 3000000, 3000000, 3000000, 3000000)),
            ("candidate", (0, 0, 0, 0, 1500000)),
        ):
            rows = [
                {
                    "name": name,
                    "track_id": track,
                    "ts": step * 3000000000,
                    "value": value,
                }
                for track, name in enumerate(compare.RAILS)
                for step, value in enumerate(values)
            ]
            rails = compare.integrate_rails(rows, 1000000000, 11000000000, 0)
            self.assertEqual(rails[0]["energy_joules_bounds"][0], 0)
            self.assertEqual(rails[0]["energy_joules"], 2 if arm == "reference" else 1)
            for window in windows:
                if window["metadata"]["arm"] == arm:
                    window["trace"]["rails"] = rails
        result = compare.summarize(plan, windows)
        self.assertFalse(result["energy_criterion_observed"])
        self.assertTrue(
            all(
                not pair["modem_reduction_above_sampling_and_clock_uncertainty"]
                for pair in result["pairs"]
            )
        )

    def test_ambiguous_duplicate_gap_reset_and_bad_counters(self):
        for mutation in (
            "track",
            "timestamp",
            "gap",
            "reset",
            "negative",
            "nan",
            "missing",
        ):
            rows = rail_rows()
            if mutation == "track":
                rows[1]["track_id"] = "3"
            elif mutation == "timestamp":
                rows[1]["ts"] = rows[0]["ts"]
            elif mutation == "gap":
                rows[1]["ts"] = "4000000001"
            elif mutation == "reset":
                rows[2]["value"] = "0"
            elif mutation == "negative":
                rows[0]["value"] = "-1"
            elif mutation == "nan":
                rows[0]["value"] = "nan"
            else:
                rows = rows[:5]
            with (
                self.subTest(mutation=mutation),
                self.assertRaises(compare.InvalidWindow),
            ):
                compare.integrate_rails(rows, 500000000, 3500000000, 0)

    def test_no_extrapolation_even_for_clock_uncertainty(self):
        with self.assertRaisesRegex(compare.InvalidWindow, "rail_coverage"):
            compare.integrate_rails(rail_rows(), 0, 4000000000, 1)

    def test_trace_errors_clock_domain_and_missing_rails(self):
        interval = {
            "start_boottime_ns": 1,
            "end_boottime_ns": 2,
            "clock_uncertainty_ms": 0,
        }
        for responses in (
            ([{"n": "1"}],),
            ([{"n": "0"}], [{"ts": "1", "clock_value": "2"}]),
            ([{"n": "0"}], [{"ts": "1", "clock_value": "1"}], []),
        ):
            with tempfile.TemporaryDirectory() as root:
                trace = Path(root) / "trace"
                trace.write_bytes(b"test-trace")
                with (
                    patch.object(compare, "trace_query", side_effect=responses),
                    self.assertRaises(compare.InvalidWindow),
                ):
                    compare.analyze_trace(Path("trusted-tool"), trace, interval)


class SeriesTests(unittest.TestCase):
    def test_paired_reversed_series_never_completes_goal(self):
        plan, windows = series()
        result = compare.summarize(plan, windows)
        self.assertEqual([p["app_ratio"] for p in result["pairs"]], [0.04] * 3)
        self.assertTrue(result["energy_criterion_observed"])
        self.assertFalse(result["goal_complete"])

    def test_one_bad_pair_prevents_claim(self):
        plan, windows = series()
        windows[-1]["accounting"]["app_mah_per_hour"] = 11
        self.assertFalse(compare.summarize(plan, windows)["energy_criterion_observed"])

    def test_failed_probe_is_functional_failure_not_excluded_low_energy(self):
        plan, windows = series()
        windows[-1]["functionality_passed"] = False
        result = compare.summarize(plan, windows)
        self.assertFalse(result["functionality_passed"])
        self.assertFalse(result["energy_criterion_observed"])
        self.assertEqual(len(result["pairs"]), 3)

    def test_uncorroborated_modem_reduction_prevents_claim(self):
        plan, windows = series()
        windows[-1]["trace"]["rails"][0]["energy_joules_bounds"] = [999, 1001]
        self.assertFalse(compare.summarize(plan, windows)["energy_criterion_observed"])

    def test_frontend_reduction_cannot_hide_a_modem_increase(self):
        plan, windows = series()
        rails = windows[-1]["trace"]["rails"]
        rails[0]["energy_joules_bounds"] = [1099, 1101]
        rails[0]["mean_mw"] = 1100 / 1.8
        result = compare.summarize(plan, windows)
        self.assertTrue(
            result["pairs"][-1][
                "combined_rail_reduction_above_sampling_and_clock_uncertainty"
            ]
        )
        self.assertFalse(
            result["pairs"][-1]["modem_reduction_above_sampling_and_clock_uncertainty"]
        )
        self.assertFalse(result["energy_criterion_observed"])

    def test_invalid_unmatched_reordered_or_reused_windows(self):
        for mutation in (
            "invalid",
            "order",
            "apk",
            "identity",
            "policy",
            "signal",
            "temperature",
            "uid",
            "settle",
            "reused",
            "overlap",
            "boot",
            "zero_baseline",
        ):
            plan, windows = series()
            last = windows[-1]
            if mutation == "invalid":
                last["valid"] = False
            elif mutation == "order":
                last["metadata"]["arm"] = "reference"
            elif mutation == "apk":
                last["metadata"]["identity"]["apk"] = sha("wrong")
            elif mutation == "identity":
                last["metadata"]["identity"]["profile"] = sha("wrong")
            elif mutation == "policy":
                last["metadata"]["start"]["context"]["power_save"] = True
            elif mutation in ("signal", "temperature"):
                last["metadata"]["start"]["context"][
                    "rsrp_dbm" if mutation == "signal" else "temperature_tenths_c"
                ] = -110 if mutation == "signal" else 350
            elif mutation == "uid":
                last["accounting"]["uid"] += 1
            elif mutation == "settle":
                last["metadata"]["settle_seconds"] += 1
            elif mutation == "reused":
                last["accounting"]["raw_report_sha256"] = windows[0]["accounting"][
                    "raw_report_sha256"
                ]
            elif mutation == "overlap":
                last["accounting"]["start_boottime_ns"] = windows[0]["accounting"][
                    "start_boottime_ns"
                ]
            elif mutation == "boot":
                last["metadata"]["start"]["boot_id"] = (
                    "b1234567-89ab-4cde-8f01-234567890abc"
                )
            else:
                windows[0]["accounting"]["app_mah_per_hour"] = 0
            with (
                self.subTest(mutation=mutation),
                self.assertRaises(compare.InvalidWindow),
            ):
                compare.summarize(plan, windows)


class InputTests(unittest.TestCase):
    def bundle(self, root, arm="reference", offset=0):
        metadata, reports = fixture(arm, offset)
        for name, report in zip(("start.pb", "end.pb"), reports):
            (root / name).write_bytes(report.SerializeToString())
        (root / "window.json").write_text(json.dumps(metadata))
        (root / "trace.pftrace").write_text(
            json.dumps(
                {
                    "synthetic_not_device_evidence": True,
                    "start_ns": (10000000 + offset) * 1000000,
                    "end_ns": (11800000 + offset) * 1000000,
                    "factor": 1 if arm == "reference" else 0.1,
                }
            )
        )
        processor = root / "processor"
        processor.write_text(f"""#!{sys.executable}
import csv
import json
import sys
from pathlib import Path
sql = sys.argv[-1]
if "FROM stats" in sql:
    print('"n"\\n0')
elif "clock_snapshot" in sql:
    print('"ts","clock_value"\\n0,0')
else:
    trace = json.loads(Path(sys.argv[-2]).read_text())
    out = csv.writer(sys.stdout)
    out.writerow(("name", "track_id", "ts", "value"))
    for track, name in enumerate(("power.rails.modem", "power.rails.radio.frontend"), 1):
        for ts in range(trace["start_ns"] - 1000000000, trace["end_ns"] + 1000000001, 1000000000):
            out.writerow((name, track, ts, ts / 1000 * trace["factor"]))
""")
        processor.chmod(0o700)
        return processor

    def test_complete_synthetic_cli_series(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            names = ["slot-" + str(n) for n in range(6)]
            processor = None
            for index, (name, arm) in enumerate(zip(names, compare.ORDER)):
                directory = root / name
                directory.mkdir()
                processor = self.bundle(directory, arm, index * 2400000)
            plan = {
                "schema_version": 1,
                "windows": names,
                "reference_apk": sha("reference"),
                "candidate_apk": sha("candidate"),
                "window_seconds": 1800,
            }
            (root / "plan.json").write_text(json.dumps(plan))
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "series",
                    str(root),
                    "--trace-processor",
                    str(processor),
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stdout.decode())
            value = json.loads(result.stdout)
            self.assertEqual(len(value["windows"]), 6)
            self.assertEqual(value["app_ratio_median"], 0.04)
            self.assertTrue(value["energy_criterion_observed"])
            self.assertFalse(value["goal_complete"])
            self.assertEqual(
                value["plan_sha256"],
                hashlib.sha256((root / "plan.json").read_bytes()).hexdigest(),
            )

    def test_cli_window_round_trip_and_failed_probe_exit(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = self.bundle(root)
            command = [
                sys.executable,
                str(path),
                "window",
                str(root),
                "--trace-processor",
                str(processor),
            ]
            result = subprocess.run(command, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout.decode())
            value = json.loads(result.stdout)
            self.assertTrue(value["valid"])
            self.assertEqual(value["accounting"]["app_mah_per_hour"], 100)
            self.assertEqual(value["trace"]["rails"][0]["mean_mw"], 1000)
            self.assertFalse(value["goal_complete"])
            self.assertEqual(
                value["trace_processor_sha256"],
                hashlib.sha256(processor.read_bytes()).hexdigest(),
            )
            metadata = json.loads((root / "window.json").read_text())
            metadata["probes"]["inbound_after"] = False
            (root / "window.json").write_text(json.dumps(metadata))
            result = subprocess.run(command, capture_output=True, check=False)
            value = json.loads(result.stdout)
            self.assertEqual(result.returncode, 1)
            self.assertTrue(value["valid"])
            self.assertFalse(value["functionality_passed"])
            self.assertEqual(value["accounting"]["app_mah_per_hour"], 100)

    def test_collector_failure_and_pending_attempt_keep_energy_but_cannot_pass(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = self.bundle(root)
            command = [
                sys.executable,
                str(path),
                "window",
                str(root),
                "--trace-processor",
                str(processor),
            ]
            (root / "capture-identity.json").write_text("{}")
            result = subprocess.run(command, capture_output=True, check=False)
            value = json.loads(result.stdout)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(value["valid"])
            self.assertIn("collector_attempt_incomplete", value["collector_errors"])
            self.assertEqual(value["accounting"]["app_mah_per_hour"], 100)
            collector = {
                "schema_version": 1,
                "captured": True,
                "restoration_verified": False,
                "cleanup_failures": [],
                "error": "private-do-not-echo",
            }
            (root / "result.json").write_text(json.dumps(collector))
            result = subprocess.run(command, capture_output=True, check=False)
            value = json.loads(result.stdout)
            self.assertFalse(value["valid"])
            self.assertEqual(value["accounting"]["app_mah_per_hour"], 100)
            self.assertNotIn(b"private-do-not-echo", result.stdout + result.stderr)
            collector["restoration_verified"] = True
            del collector["error"]
            (root / "result.json").write_text(json.dumps(collector))
            result = subprocess.run(command, capture_output=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout.decode())
            value = json.loads(result.stdout)
            self.assertTrue(value["valid"])
            self.assertIn("collector_result_sha256", value)

    def test_cli_series_retains_every_invalid_slot(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            names = ["slot-" + str(n) for n in range(6)]
            (root / "plan.json").write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "windows": names,
                        "reference_apk": sha("reference"),
                        "candidate_apk": sha("candidate"),
                        "window_seconds": 1800,
                    }
                )
            )
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "series",
                    str(root),
                    "--trace-processor",
                    sys.executable,
                ],
                capture_output=True,
                check=False,
            )
            value = json.loads(result.stdout)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(value["valid"])
            self.assertEqual(len(value["windows"]), 6)
            self.assertTrue(all(not window["valid"] for window in value["windows"]))

    def test_processor_output_is_bounded(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = root / "processor"
            processor.write_text(
                f"#!{sys.executable}\nimport sys\nsys.stdout.write('x' * {compare.MAX_PROCESSOR_OUTPUT + 1})\n"
            )
            processor.chmod(0o700)
            with self.assertRaisesRegex(
                compare.InvalidWindow, "trace_processor|trace_output_bounds"
            ):
                compare.trace_query(processor, root / "unused", "unused")

    def test_processor_address_space_is_bounded(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = root / "processor"
            processor.write_text(f"""#!{sys.executable}
import sys
try:
    allocation = bytearray({compare.MAX_PROCESSOR_MEMORY})
except MemoryError:
    print('n\\n0')
else:
    sys.exit(2)
""")
            processor.chmod(0o700)
            self.assertEqual(
                compare.trace_query(processor, root / "unused", "unused"), [{"n": "0"}]
            )

    def test_oversized_csv_field_is_a_redacted_validation_error(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = root / "processor"
            processor.write_text(f"#!{sys.executable}\nprint('n\\n' + 'x' * 200000)\n")
            processor.chmod(0o700)
            with self.assertRaisesRegex(compare.InvalidWindow, "trace_csv"):
                compare.trace_query(processor, root / "unused", "unused")

    def test_non_string_boot_id_is_json_failure_without_traceback(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            processor = self.bundle(root)
            metadata = json.loads((root / "window.json").read_text())
            metadata["start"]["boot_id"] = 123
            (root / "window.json").write_text(json.dumps(metadata))
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "window",
                    str(root),
                    "--trace-processor",
                    str(processor),
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertEqual(json.loads(result.stdout)["error"], "boot_id")
            self.assertNotIn(b"Traceback", result.stderr)

    def run_series_main(self, plan, responses):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "plan.json").write_text(json.dumps(plan))
            output = io.StringIO()
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        str(path),
                        "series",
                        str(root),
                        "--trace-processor",
                        sys.executable,
                    ],
                ),
                patch.object(compare, "safe_window", side_effect=responses) as analyze,
                patch.object(compare.signal, "signal"),
                patch.object(compare.signal, "alarm"),
                contextlib.redirect_stdout(output),
            ):
                code = compare.main()
            return code, json.loads(output.getvalue()), analyze.call_count

    def test_missing_plan_fields_preserve_all_slot_outcomes(self):
        for missing in ("schema_version", "reference_apk", "candidate_apk"):
            plan, windows = series()
            plan["windows"] = ["slot-" + str(index) for index in range(6)]
            del plan[missing]
            code, result, calls = self.run_series_main(plan, windows)
            self.assertEqual((code, calls), (1, 6))
            self.assertEqual(len(result["windows"]), 6)
            self.assertTrue(all(window["valid"] for window in result["windows"]))
            self.assertFalse(result["valid"])

    def test_deadline_stops_series_preserving_completed_and_unfinished_slots(self):
        plan, windows = series()
        plan["windows"] = ["slot-" + str(index) for index in range(6)]
        code, result, calls = self.run_series_main(
            plan, [windows[0], compare.AnalysisDeadline()]
        )
        self.assertEqual((code, calls), (1, 2))
        self.assertEqual(result["error"], "analysis_timeout")
        self.assertTrue(result["windows"][0]["valid"])
        self.assertEqual(result["windows"][1]["error"], "analysis_timeout")
        self.assertTrue(
            all(window["error"] == "not_analyzed" for window in result["windows"][2:])
        )
        with (
            patch.object(
                compare, "analyze_window", side_effect=compare.AnalysisDeadline()
            ),
            self.assertRaises(compare.AnalysisDeadline),
        ):
            compare.safe_window(Path("unused"), Path("unused"))

    def test_no_symlink_fifo_oversized_or_duplicate_json_input(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            regular = root / "data"
            regular.write_bytes(b"123")
            link = root / "link"
            link.symlink_to(regular)
            with self.assertRaises(OSError):
                compare.read_file(link, 100)
            fifo = root / "fifo"
            os.mkfifo(fifo)
            with self.assertRaises(compare.InvalidWindow):
                compare.read_file(fifo, 100)
            with self.assertRaises(compare.InvalidWindow):
                compare.read_file(regular, 2)
            regular.write_text('{"private":1,"private":2}')
            with self.assertRaisesRegex(compare.InvalidWindow, "duplicate_json_key"):
                compare.read_json(regular)

    def test_cli_failure_is_json_and_redacted(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "window.json").write_text('{"private":"do-not-echo"}')
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "window",
                    str(root),
                    "--trace-processor",
                    sys.executable,
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["valid"])
            self.assertNotIn(b"do-not-echo", result.stdout + result.stderr)
            self.assertNotIn(b"Traceback", result.stderr)

    def test_cli_traversal_rejected_without_running_processor(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "plan.json").write_text(
                json.dumps({"windows": ["../private", "2", "3", "4", "5", "6"]})
            )
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "series",
                    str(root),
                    "--trace-processor",
                    sys.executable,
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(json.loads(result.stdout)["error"], "plan_windows")


unittest.main()

import contextlib
import copy
import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

path = Path(
    os.environ.get(
        "P2P_VPN_BATTERY_PAIRED",
        Path(__file__).parent.parent / "scripts/android-battery-paired.py",
    )
)
spec = importlib.util.spec_from_file_location("paired", path)
paired = importlib.util.module_from_spec(spec)
spec.loader.exec_module(paired)


def endpoints():
    start = {
        "energy": {
            "uid": 10303,
            "report_version": 36,
            "parcel_version": 216,
            "uid_consumption_mah": 10.0,
            "scope": {
                "start_clock_time_ms": 1234,
                "start_count": 1,
                "total_realtime_ms": 1000,
                "battery_realtime_ms": 1000,
                "screen_off_realtime_ms": 1000,
            },
        },
        "context": {
            "apk_sha256": "a",
            "profile_sha256": "b",
            "network_identity_sha256": "c",
            "generation": 1,
            "application_pid": 123,
            "wifi_identity_sha256": "d",
            "wifi_frequency_mhz": 5180,
            "connected": True,
            "busy": False,
            "pairing": False,
            "thermal_status": 0,
            "wifi_rssi_dbm": -33,
            "power_save": False,
            "device_idle": "IDLE",
            "underlay": {
                "kind": "wifi",
                "validated": True,
                **dict.fromkeys(paired.CHANGES, 0),
            },
            "paths": {
                "connected_peers": 5,
                "packet_plane_quic_sessions": 5,
                **dict.fromkeys(
                    (
                        "outbound_quic_datagram_packets",
                        "outbound_owned_quic_datagram_packets",
                        "outbound_owned_udp_datagram_packets",
                        "outbound_direct_tcp_stream_packets",
                    ),
                    0,
                ),
            },
            "counters": [
                [f"{key} 0" for key in (*paired.COUNTERS, *paired.PAYLOAD_COUNTERS)]
            ],
        },
    }
    end = copy.deepcopy(start)
    end["energy"]["uid_consumption_mah"] = 10.5
    for key in ("total_realtime_ms", "battery_realtime_ms", "screen_off_realtime_ms"):
        end["energy"]["scope"][key] += 1800000
    return start, end


def rows(rates=None):
    baseline = paired.validate_window(*endpoints(), 1800, 5)
    context = baseline["comparison_context"]
    return [
        {
            "build": build,
            "valid": True,
            "mah_per_hour": rate,
            "comparison_context": copy.deepcopy(context),
            "wifi_rssi_dbm": baseline["wifi_rssi_dbm"],
        }
        for build, rate in zip(paired.ORDER, rates or [1.0] * 6, strict=True)
    ]


class PairedTests(unittest.TestCase):
    def test_rate_uses_deltas_not_historical_total(self):
        self.assertEqual(
            paired.validate_window(*endpoints(), 1800, 5)["mah_per_hour"], 1.0
        )

    def test_energy_reset_identity_missing_and_invalid_charge(self):
        for key, value in (
            ("uid", 7),
            ("report_version", 37),
            ("uid_consumption_mah", None),
            ("uid_consumption_mah", float("nan")),
            ("uid_consumption_mah", 9.0),
        ):
            start, end = endpoints()
            end["energy"][key] = value
            with (
                self.subTest(key=key, value=value),
                self.assertRaises(paired.InvalidTrial),
            ):
                paired.validate_window(start, end, 1800, 5)
        for key in ("start_clock_time_ms", "start_count"):
            start, end = endpoints()
            end["energy"]["scope"][key] += 1
            with self.assertRaises(paired.InvalidTrial):
                paired.validate_window(start, end, 1800, 5)

    def test_screen_wakeup_and_charging_accounting_rejected(self):
        for key in ("screen_off_realtime_ms", "battery_realtime_ms"):
            start, end = endpoints()
            end["energy"]["scope"][key] -= 2000
            with self.assertRaises(paired.InvalidTrial):
                paired.validate_window(start, end, 1800, 5)

    def test_changed_identity_build_process_and_radio_rejected(self):
        for key in (
            "apk_sha256",
            "profile_sha256",
            "network_identity_sha256",
            "generation",
            "application_pid",
            "wifi_identity_sha256",
            "wifi_frequency_mhz",
        ):
            start, end = endpoints()
            end["context"][key] = "changed"
            with self.subTest(key=key), self.assertRaises(paired.InvalidTrial):
                paired.validate_window(start, end, 1800, 5)

    def test_network_transition_cannot_hide_behind_wifi_endpoint(self):
        for key in paired.CHANGES:
            start, end = endpoints()
            end["context"]["underlay"][key] += 1
            with self.assertRaises(paired.InvalidTrial):
                paired.validate_window(start, end, 1800, 5)

    def test_topology_signal_thermal_and_application_work_rejected(self):
        for area, key, value in (
            ("paths", "connected_peers", 4),
            ("paths", "packet_plane_quic_sessions", 4),
            ("paths", "outbound_owned_quic_datagram_packets", 1),
            (None, "wifi_rssi_dbm", -40),
            (None, "thermal_status", 1),
            (None, "pairing", True),
        ):
            start, end = endpoints()
            target = end["context"] if area is None else end["context"][area]
            target[key] = value
            with self.subTest(key=key), self.assertRaises(paired.InvalidTrial):
                paired.validate_window(start, end, 1800, 5)

    def test_recovery_is_retained_as_outcome_not_cherry_picked(self):
        start, end = endpoints()
        end["context"]["counters"][0][0] = f"{paired.COUNTERS[0]} 12"
        value = paired.validate_window(start, end, 1800, 5)
        self.assertTrue(value["valid"])
        self.assertEqual(value["secondary_counter_deltas"][0][paired.COUNTERS[0]], 12)

    def test_missing_counter_is_null_not_zero_and_reset_rejected(self):
        start, end = endpoints()
        start["context"]["counters"][0] = [
            line
            for line in start["context"]["counters"][0]
            if not line.startswith(paired.COUNTERS[-1] + " ")
        ]
        value = paired.validate_window(start, end, 1800, 5)
        self.assertIsNone(value["secondary_counter_deltas"][0][paired.COUNTERS[-1]])
        start, end = endpoints()
        start["context"]["counters"][0][0] = f"{paired.COUNTERS[0]} 12"
        with self.assertRaises(paired.InvalidTrial):
            paired.validate_window(start, end, 1800, 5)

    def test_invalid_or_missing_trials_invalidate_whole_series(self):
        values = rows()
        self.assertEqual(paired.summarize(values[:-1])["status"], "inconclusive")
        values[2]["valid"] = False
        self.assertEqual(paired.summarize(values)["status"], "inconclusive")

    def test_fluctuating_pairs_do_not_claim_consistent_improvement(self):
        rates = (2, 1, 3, 2, 2, 1)
        value = paired.summarize(rows(rates))
        self.assertEqual(value["candidate_over_reference_by_pair"], [0.5, 1.5, 0.5])
        self.assertEqual(value["status"], "inconclusive")
        self.assertFalse(value["all_pairs_lower"])
        self.assertFalse(value["goal_complete"])

    def test_even_ten_x_series_is_not_full_goal_completion(self):
        value = paired.summarize(
            rows([0.1 if build == "candidate" else 1.0 for build in paired.ORDER])
        )
        self.assertTrue(value["energy_target_observed_in_every_pair"])
        self.assertFalse(value["goal_complete"])

    def test_unresolvable_energy_is_inconclusive(self):
        self.assertEqual(paired.summarize(rows([0.0] * 6))["status"], "inconclusive")

    def test_restore_attempts_remaining_steps_after_failure(self):
        phone = paired.Phone("device")
        phone.fallback = (42, "owned")
        phone.device = Mock()
        phone.device.restore.side_effect = paired.InvalidTrial("offline")
        self.assertEqual(phone.restore(), ["battery_reporting"])
        phone.device.shell.assert_called_once_with("input", "keyevent", "KEYCODE_SLEEP")
        self.assertEqual(phone.fallback, (42, "owned"))

    def test_failed_trials_retained_and_interruptions_restore(self):
        class FakePhone:
            def preflight(self):
                pass

            def open(self):
                pass

            def shell(self, *args):
                return "e" * 64 if "sha256sum" in args else "test"

            def rpc(self, command):
                return {
                    "snapshot": {
                        "connected": False,
                        "always_on": False,
                        "lockdown": False,
                        "pairing": {"code": None, "candidate_peer": None},
                    }
                }

            def restore(self):
                restores.append(True)
                return []

        for error, expected_count in (
            (paired.InvalidTrial("bad conditions"), 6),
            (KeyboardInterrupt(), 1),
        ):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                plan = {
                    "serial": "device",
                    "reference": {"apk": "/a.apk", "sha256": "a" * 64},
                    "candidate": {"apk": "/b.apk", "sha256": "b" * 64},
                    "expected_peers": 5,
                    "settle_seconds": 300,
                    "window_seconds": 1800,
                }
                (root / "plan.json").write_text(json.dumps(plan))
                restores = []
                outcomes = [error] + [{"valid": True, "mah_per_hour": 1.0}] * 5
                with (
                    patch.object(
                        paired.capture,
                        "device_lock",
                        return_value=contextlib.nullcontext(),
                    ),
                    patch.object(paired, "Phone", return_value=FakePhone()),
                    patch.object(paired, "trial", side_effect=outcomes),
                    patch(
                        "sys.argv",
                        [
                            "paired",
                            "--plan",
                            str(root / "plan.json"),
                            "--output",
                            str(root / "out"),
                        ],
                    ),
                    patch("builtins.print"),
                ):
                    self.assertEqual(paired.main(), 1)
                self.assertEqual(
                    len(list((root / "out").glob("trial-*/result.json"))),
                    expected_count,
                )
                self.assertEqual(len(restores), expected_count + 1)
                self.assertEqual(
                    json.loads((root / "out/summary.json").read_text())["status"],
                    "inconclusive",
                )

    def test_timing_plan_cannot_shrink_into_unreliable_short_capture(self):
        plan = {
            "serial": "device",
            "reference": {"apk": "/a.apk", "sha256": "a" * 64},
            "candidate": {"apk": "/b.apk", "sha256": "b" * 64},
            "expected_peers": 5,
            "settle_seconds": 300,
            "window_seconds": 1800,
        }
        paired.validate_plan(plan)
        for key, value in (
            ("settle_seconds", 0),
            ("window_seconds", 600),
            ("expected_peers", True),
            ("serial", "device;command"),
        ):
            altered = {**plan, key: value}
            with self.assertRaises(paired.InvalidTrial):
                paired.validate_plan(altered)

    def test_cross_trial_context_mismatch_is_inconclusive(self):
        for key in rows()[0]["comparison_context"]:
            values = rows()
            values[3]["comparison_context"][key] = "different"
            with self.subTest(key=key):
                self.assertEqual(paired.summarize(values)["status"], "inconclusive")
        values = rows()
        del values[0]["comparison_context"]
        self.assertEqual(paired.summarize(values)["status"], "inconclusive")

    def test_incoming_payload_and_missing_payload_counters_rejected(self):
        for key in paired.PAYLOAD_COUNTERS:
            for changed in (False, True):
                start, end = endpoints()
                lines = end["context"]["counters"][0]
                lines[:] = [line for line in lines if not line.startswith(key + " ")]
                if changed:
                    lines.append(key + " 1")
                with (
                    self.subTest(key=key, changed=changed),
                    self.assertRaises(paired.InvalidTrial),
                ):
                    paired.validate_window(start, end, 1800, 5)

    def test_unowned_battery_reporting_is_not_reset(self):
        phone = paired.Phone("device")
        phone.device = Mock()
        self.assertEqual(phone.restore(), [])
        phone.device.restore.assert_not_called()
        phone.device.shell.assert_called_once_with("input", "keyevent", "KEYCODE_SLEEP")

    def test_fallback_is_armed_before_battery_override(self):
        phone = paired.Phone("device")
        phone.device = Mock()
        phone.device.fallback.return_value = 42
        phone.begin_accounting(2100)
        tag = phone.fallback[1]
        self.assertTrue(tag.startswith("p2p-vpn-battery-wifi-"))
        self.assertEqual(
            phone.device.method_calls,
            [
                unittest.mock.call.fallback(tag, 2400),
                unittest.mock.call.begin_override(42, tag),
            ],
        )
        self.assertEqual(phone.restore(), [])
        phone.device.restore.assert_called_once_with(42, tag)
        self.assertIsNone(phone.fallback)

    def test_override_failure_retains_owned_fallback_for_restoration(self):
        phone = paired.Phone("device")
        phone.device = Mock()
        phone.device.fallback.return_value = 42
        phone.device.begin_override.side_effect = paired.InvalidTrial("offline")
        with self.assertRaises(paired.InvalidTrial):
            phone.begin_accounting(2100)
        self.assertEqual(phone.fallback[0], 42)
        self.assertEqual(phone.restore(), [])
        phone.device.restore.assert_called_once()

    def test_relative_apk_paths_rejected(self):
        plan = {
            "serial": "device",
            "reference": {"apk": "a.apk", "sha256": "a" * 64},
            "candidate": {"apk": "/b.apk", "sha256": "b" * 64},
            "expected_peers": 5,
            "settle_seconds": 300,
            "window_seconds": 1800,
        }
        with self.assertRaises(paired.InvalidTrial):
            paired.validate_plan(plan)

    def test_paired_signal_drift_is_inconclusive(self):
        values = rows()
        values[1]["wifi_rssi_dbm"] = [-60, -60]
        self.assertEqual(paired.summarize(values)["status"], "inconclusive")

    def test_preflight_rejects_existing_or_pending_override(self):
        for outputs in (
            ["UPDATES STOPPED USB powered: true"],
            ["USB powered: false"],
            ["USB powered: true", "owned.armed"],
        ):
            phone = paired.Phone("device")
            phone.device = Mock()
            phone.device.shell.side_effect = outputs
            with self.subTest(outputs=outputs), self.assertRaises(paired.InvalidTrial):
                phone.preflight()
            phone.device.usb.assert_called_once()

    def test_fallback_setup_failure_cannot_enable_simulation(self):
        phone = paired.Phone("device")
        phone.device = Mock()
        phone.device.fallback.side_effect = paired.capture.compare.InvalidWindow(
            "unavailable"
        )
        with self.assertRaises(ValueError):
            phone.begin_accounting(2100)
        phone.device.begin_override.assert_not_called()
        self.assertIsNone(phone.fallback)

    def test_raw_report_retained_before_decoding(self):
        phone = paired.Phone("device")
        phone.device = Mock()
        phone.device.adb.return_value = (0, b"raw report")
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(
                paired.decoder,
                "decode",
                side_effect=paired.decoder.InvalidReport("bad report"),
            ),
        ):
            root = Path(directory)
            with self.assertRaises(paired.decoder.InvalidReport):
                phone.energy(10303, root, "start.pb")
            self.assertEqual((root / "start.pb").read_bytes(), b"raw report")

    def test_running_or_always_on_phone_rejected_before_mutation(self):
        plan = {
            "serial": "device",
            "reference": {"apk": "/a.apk", "sha256": "a" * 64},
            "candidate": {"apk": "/b.apk", "sha256": "b" * 64},
            "expected_peers": 5,
            "settle_seconds": 300,
            "window_seconds": 1800,
        }
        for flag in ("connected", "always_on", "lockdown"):
            phone = Mock()
            snapshot = {
                "connected": False,
                "always_on": False,
                "lockdown": False,
                "pairing": {"code": None, "candidate_peer": None},
            }
            snapshot[flag] = True
            phone.rpc.return_value = {"snapshot": snapshot}
            with (
                tempfile.TemporaryDirectory() as directory,
                patch.object(paired, "Phone", return_value=phone),
                self.subTest(flag=flag),
            ):
                with self.assertRaises(paired.InvalidTrial):
                    paired.run(plan, Path(directory) / "out")
                phone.open.assert_not_called()
                phone.adb.assert_not_called()
                phone.begin_accounting.assert_not_called()


unittest.main()

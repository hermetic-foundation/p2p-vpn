import base64
import contextlib
import copy
import importlib.util
import io
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch

path = Path(
    os.environ.get(
        "P2P_VPN_CELLULAR_CAPTURE",
        Path(__file__).parent.parent / "scripts/android-cellular-capture.py",
    )
)
spec = importlib.util.spec_from_file_location("capture_tool", path)
tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tool)


def workload():
    return {
        "schema_version": 1,
        "network_name": "battery-trial",
        "phone_peer_id": "12D3KooWPhoneTestPeerId123456",
        "phone_address": "10.42.0.2",
        "peer_address": "10.42.0.1",
        "authorized_peer_ids": ["12D3KooWLinuxTestPeerId123456"],
        "counterpart_builds": {"linux": "a" * 64},
        "diagnostic_overrides": False,
    }


def radio_state(first="rsrp=-85 ssRsrp=2147483647", second="rsrp=-95"):
    return (
        "last known state:\nPhone Id=0\nmSignalStrength="
        + first
        + "\nPhone Id=1\nmSignalStrength="
        + second
        + "\nmPhoneCapability=null\nmActiveDataSubId=1\nmDefaultPhoneId=0\nmDefaultSubId=1\nlocal logs:\n"
    )


THERMAL = "Thermal Status: 0\nCurrent temperatures from HAL:\nTemperature{mValue=28.1, mType=2, mName=battery, foo}\nCurrent cooling devices"


def initialize(root):
    reference, candidate, source = (
        root / "reference.apk",
        root / "candidate.apk",
        root / "workload.json",
    )
    reference.write_bytes(b"reference-test-only")
    candidate.write_bytes(b"candidate-test-only")
    source.write_text(json.dumps(workload()))
    directory = root / "series"
    tool.initialize(directory, reference, candidate, source, 1800, 300)
    return directory


class Clock:
    def __init__(self):
        self.seconds = 0
        self.events = []
        self.fail_sleep = None

    def sleep(self, seconds):
        self.events.append(("sleep", seconds))
        if self.fail_sleep == seconds:
            raise tool.InterruptedCapture()
        self.seconds += seconds


class FakeDevice:
    def __init__(self, clock):
        self.clock = clock
        self.events = clock.events
        self.frozen = False
        self.restore_fails = False
        self.bad_report = False
        self.fail_after_probe = False
        self.identity_changed = False
        self.reads = 0
        self.start_decode_delay = 0
        self.raise_after_probe = False
        self.interrupt_after_probe = False
        self.cold_delay = 0
        self.steady_delay = 0
        self.cold_error = None
        self.steady_received = 3
        self.apk = tool.sha(b"reference-test-only")

    def preflight(self, _workload):
        self.events.append(("preflight",))

    def identities(self, _workload):
        self.events.append(("identities",))
        self.reads += 1
        identity = {
            key: tool.sha(key.encode())
            for key in ("device", "os_build", "profile", "peer_inventory")
        }
        identity["apk"] = self.apk
        if self.identity_changed and self.reads >= 3:
            identity["profile"] = "b" * 64
        return identity, {"test_only": True}

    def uid(self):
        return 10304

    def probe(self, _workload, inbound):
        self.events.append(("probe", inbound))
        raise AssertionError("capture must not use the legacy diagnostic probe")

    def wake_probe(self, workload, inbound):
        self.events.append(("wake_probe", inbound))
        if self.raise_after_probe and self.clock.seconds > 1800:
            raise OSError("private-probe-failure")
        if self.interrupt_after_probe and self.clock.seconds > 1800:
            raise tool.InterruptedCapture()
        return tool.Device.wake_probe(self, workload, inbound)

    def _wake_ping(self, workload, inbound, count, wait, timeout):
        def run(*_args, **_kwargs):
            self.events.append(
                ("cold_packet" if count == 1 else "steady_probe", inbound)
            )
            if count == 1:
                self.clock.seconds += self.cold_delay
                if self.cold_error and self.clock.seconds > 1800:
                    raise self.cold_error
            else:
                self.clock.seconds += self.steady_delay
            received = count if count == 1 else self.steady_received
            if count == 1 and self.fail_after_probe and self.clock.seconds > 1800:
                received = 0
            raw = f"{count} packets transmitted, {received} received, 0% packet loss\n"
            return (0 if received else 1), raw.encode()

        with (
            patch.object(tool, "bounded_run", side_effect=run),
            patch.object(self, "adb", side_effect=run),
        ):
            return tool.Device._wake_ping(self, workload, inbound, count, wait, timeout)

    def fallback(self, tag, seconds):
        self.events.append(("fallback", tag, seconds))
        return 321

    def begin_override(self, _pid, _tag):
        self.shell("dumpsys", "battery", "unplug", "-f")

    def shell(self, *args, **_kwargs):
        self.events.append(("shell", *args))
        if args == ("dumpsys", "battery", "unplug", "-f"):
            self.frozen = True
        return ""

    def adb(self, *args, **_kwargs):
        self.events.append(("adb", *args))
        return 0, b"456\n"

    def context(self, _workload, _evidence):
        self.events.append(("context",))
        return {
            "app_pid": 1234,
            "runtime_generation": 1,
            "screen_interactive": False,
            "wifi_enabled": False,
            "underlay_kind": "cellular",
            "underlay_validated": True,
            "underlay_generation": "c" * 64,
            "accounting_mode": "simulated_on_battery",
            "physical_usb_powered": True,
            "power_save": False,
            "device_idle": "INACTIVE",
            "thermal_status": 0,
            "temperature_tenths_c": 280,
            "rsrp_dbm": -85,
            "authorized_peers": 1,
            "payload_sent_packets": 10,
            "payload_received_packets": 12,
        }

    def endpoint(self, output, name, context, uid):
        self.events.append(("endpoint", name))
        message = tool.compare.decoder.report_type()()
        stats = message.batterystats
        stats.report_version, stats.parcel_version = 36, 216
        stats.uids.add(uid=uid).power_use_item.computed_power_mah = (
            100 if name == "start" else 150
        )
        battery = stats.system.battery
        for field in tool.compare.decoder.SCOPE_FIELDS:
            setattr(battery, field, 100000 + self.clock.seconds * 1000)
        battery.start_clock_time_ms = 1700000000000
        battery.start_count = 0
        raw = b"invalid-test-report" if self.bad_report else message.SerializeToString()
        tool.save(output, name + ".pb", raw)
        tool.compare.decoder.decode(raw, uid)
        result = {
            "boot_id": "a1234567-89ab-4cde-8f01-234567890abc",
            "before_uptime_ms": 10000000 + self.clock.seconds * 1000 - 5,
            "after_uptime_ms": 10000000 + self.clock.seconds * 1000 + 5,
            "context": context,
        }
        if name == "start":
            self.clock.seconds += self.start_decode_delay
        return result

    def retain_trace(self, output, _pid, _config_path, _trace_path, partial, _sleep):
        self.events.append(("retain_trace", partial))
        tool.save(
            output,
            "trace.partial.pftrace" if partial else "trace.pftrace",
            b"synthetic-not-a-real-trace",
        )

    def restore(self, _pid, _tag):
        self.events.append(("restore",))
        if self.restore_fails:
            raise OSError("private-error-not-to-print")
        self.frozen = False


def run_capture(directory, device, slot="01-A"):
    output = io.StringIO()
    with (
        patch.object(tool.time, "monotonic", side_effect=lambda: device.clock.seconds),
        contextlib.redirect_stdout(output),
    ):
        result = tool.capture(directory, slot, device, device.clock.sleep)
    return result, output.getvalue()


class CaptureWorkflowTests(unittest.TestCase):
    def test_delayed_wake_is_outside_energy_window_in_both_arms(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            for slot, apk in (
                ("01-A", b"reference-test-only"),
                ("02-B", b"candidate-test-only"),
            ):
                with self.subTest(slot=slot):
                    device = FakeDevice(Clock())
                    device.apk = tool.sha(apk)
                    device.cold_delay = 44
                    result, _ = run_capture(directory, device, slot)
                    self.assertTrue(result["captured"], result)
                    self.assertTrue(result["boundary_probes_passed"])
                    start = device.events.index(("endpoint", "start"))
                    end = device.events.index(("endpoint", "end"))
                    self.assertEqual(
                        device.events[start : end + 1],
                        [("endpoint", "start"), ("sleep", 1800), ("endpoint", "end")],
                    )
                    self.assertEqual(
                        [event for event in device.events if event[0] == "wake_probe"],
                        [("wake_probe", True), ("wake_probe", False)] * 2,
                    )
                    self.assertFalse(
                        any(event[0] == "probe" for event in device.events)
                    )
                    for key in (
                        "inbound_before",
                        "outbound_before",
                        "inbound_after",
                        "outbound_after",
                    ):
                        probe = tool.compare.read_json(
                            directory / slot / (key + ".json")
                        )
                        self.assertTrue(probe["cold"]["success"])
                        self.assertEqual(probe["cold"]["elapsed_seconds"], 44)
                        self.assertEqual(probe["cold"]["packet_count"], 1)
                        self.assertEqual(probe["steady"]["packet_count"], 3)

    def test_post_wake_timeout_retains_raw_failure_and_accounting(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.cold_error = tool.CommandFailure(
                "command_timeout", b"partial-cold-stdout\xff", b"private-stderr", None
            )
            result, output = run_capture(directory, device)
            self.assertTrue(result["captured"], result)
            self.assertFalse(result["boundary_probes_passed"])
            self.assertTrue(result["restoration_verified"])
            probe = tool.compare.read_json(directory / "01-A/inbound_after.json")
            self.assertEqual(probe["cold"]["error"], "command_timeout")
            self.assertEqual(
                base64.b64decode(probe["cold"]["stdout_base64"]),
                device.cold_error.stdout,
            )
            self.assertEqual(
                base64.b64decode(probe["cold"]["stderr_base64"]), b"private-stderr"
            )
            self.assertIsNone(probe["steady"])
            metadata = tool.compare.read_json(directory / "01-A/window.json")
            accounting = tool.compare.report_window(
                (directory / "01-A/start.pb").read_bytes(),
                (directory / "01-A/end.pb").read_bytes(),
                metadata,
            )
            self.assertEqual(accounting["app_mah_per_hour"], 100)
            self.assertFalse(metadata["probes"]["inbound_after"])
            self.assertNotIn("partial-cold", output)
            self.assertNotIn("private-stderr", output)

    def test_post_wake_interruption_retains_partial_probe_before_cleanup(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.cold_error = tool.CommandFailure(
                "command_interrupted", b"partial-cold", b"", None
            )
            result, _ = run_capture(directory, device)
            self.assertEqual(result["error"], "interrupted")
            self.assertTrue(result["restoration_verified"])
            probe = tool.compare.read_json(directory / "01-A/inbound_after.json")
            self.assertEqual(probe["cold"]["stdout"], "partial-cold")
            self.assertIsNone(probe["steady"])
            self.assertTrue((directory / "01-A/window.json").exists())
            self.assertTrue((directory / "01-A/end.pb").exists())

    def test_fallback_budget_includes_both_post_window_boundary_checks(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.cold_delay = tool.FIRST_WAKE_REPLY_WAIT_SECONDS
            device.steady_delay = tool.STEADY_PROBE_TIMEOUT_SECONDS
            result, _ = run_capture(directory, device)
            self.assertTrue(result["boundary_probes_passed"])
            fallback = tool.compare.read_json(directory / "01-A/fallback.json")
            self.assertEqual(fallback["seconds"], 300 + 1800 + 490)
            self.assertEqual(
                tool.FALLBACK_ALLOWANCE_SECONDS,
                360
                + 2
                * (tool.WAKE_PROBE_TIMEOUT_SECONDS + tool.STEADY_PROBE_TIMEOUT_SECONDS),
            )
            pre_window_probes = 2 * (device.cold_delay + device.steady_delay)
            self.assertGreaterEqual(
                fallback["seconds"] - (device.clock.seconds - pre_window_probes),
                360,
            )
            self.assertEqual(tool.WATCHDOG_ALLOWANCE_SECONDS, 740)

    def test_post_probe_interruption_keeps_completed_window_metadata(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.interrupt_after_probe = True
            result, _ = run_capture(directory, device)
            self.assertEqual(result["error"], "interrupted")
            metadata = tool.compare.read_json(directory / "01-A/window.json")
            self.assertEqual(metadata["uid"], 10304)
            self.assertFalse(metadata["probes"]["inbound_after"])
            self.assertTrue((directory / "01-A/start.pb").exists())
            self.assertTrue((directory / "01-A/end.pb").exists())
            self.assertFalse(device.frozen)

    def test_post_probe_exception_retains_full_window_and_uid(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.raise_after_probe = True
            result, output = run_capture(directory, device)
            self.assertTrue(result["captured"])
            self.assertFalse(result["boundary_probes_passed"])
            metadata = tool.compare.read_json(directory / "01-A/window.json")
            self.assertEqual(metadata["uid"], 10304)
            self.assertFalse(metadata["probes"]["inbound_after"])
            self.assertTrue((directory / "01-A/start-endpoint.json").exists())
            self.assertTrue((directory / "01-A/end-endpoint.json").exists())
            self.assertNotIn("private-probe-failure", output)

    def test_decode_delay_does_not_extend_accounting_interval(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.start_decode_delay = 9
            result, _ = run_capture(directory, device)
            self.assertTrue(result["captured"], result)
            self.assertIn(("sleep", 1791), device.events)

    def test_complete_workflow_and_clean_measurement_gap(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            result, _ = run_capture(directory, device)
            self.assertTrue(result["captured"])
            self.assertTrue(result["restoration_verified"])
            self.assertFalse(result["goal_complete"])
            self.assertFalse(device.frozen)
            start = device.events.index(("endpoint", "start"))
            end = device.events.index(("endpoint", "end"))
            self.assertEqual(
                device.events[start : end + 1],
                [("endpoint", "start"), ("sleep", 1800), ("endpoint", "end")],
            )
            self.assertTrue(
                all(event[0] != "probe" for event in device.events[start:end])
            )
            metadata = tool.compare.read_json(directory / "01-A/window.json")
            self.assertEqual(metadata["settle_seconds"], 300)
            tool.compare.validate_context(metadata)
            accounting = tool.compare.report_window(
                (directory / "01-A/start.pb").read_bytes(),
                (directory / "01-A/end.pb").read_bytes(),
                metadata,
            )
            self.assertEqual(accounting["app_mah_per_hour"], 100)
            for file in (directory / "01-A").iterdir():
                self.assertEqual(file.stat().st_mode & 0o777, 0o600)

    def test_interruption_restores_reporting_and_retains_attempt(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.clock.fail_sleep = 300
            result, _ = run_capture(directory, device)
            self.assertFalse(result["captured"])
            self.assertEqual(result["error"], "interrupted")
            self.assertTrue(result["restoration_verified"])
            self.assertFalse(device.frozen)
            self.assertTrue((directory / "01-A/result.json").exists())
            self.assertEqual([event[0] for event in device.events].count("restore"), 1)

    def test_usb_loss_keeps_fallback_and_reports_restoration_unverified(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.clock.fail_sleep = 1800
            device.restore_fails = True
            result, output = run_capture(directory, device)
            self.assertFalse(result["restoration_verified"])
            self.assertTrue(device.frozen)
            self.assertIn(
                "battery_restoration_unverified_fallback_retained",
                result["cleanup_failures"],
            )
            self.assertNotIn("private-error", output)
            self.assertTrue((directory / "01-A/fallback.json").exists())
            self.assertTrue((directory / "01-A/trace.partial.pftrace").exists())

    def test_malformed_raw_report_is_kept_before_decode_failure(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.bad_report = True
            result, _ = run_capture(directory, device)
            self.assertFalse(result["captured"])
            self.assertEqual(
                (directory / "01-A/start.pb").read_bytes(), b"invalid-test-report"
            )
            self.assertFalse(device.frozen)
            self.assertTrue((directory / "01-A/trace.partial.pftrace").exists())

    def test_failed_post_probe_keeps_energy_and_functional_failure(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.fail_after_probe = True
            result, _ = run_capture(directory, device)
            self.assertTrue(result["captured"])
            self.assertFalse(result["boundary_probes_passed"])
            self.assertFalse(
                tool.compare.read_json(directory / "01-A/window.json")["probes"][
                    "inbound_after"
                ]
            )
            probe = tool.compare.read_json(directory / "01-A/inbound_after.json")
            self.assertFalse(probe["cold"]["success"])
            self.assertIn("1 packets transmitted, 0 received", probe["cold"]["stdout"])
            self.assertIsNone(probe["steady"])
            self.assertEqual(
                [event for event in device.events if event[0] == "steady_probe"],
                [("steady_probe", True), ("steady_probe", False)],
            )

    def test_identity_changes_fail_without_overwriting_app_state(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.identity_changed = True
            result, _ = run_capture(directory, device)
            self.assertFalse(result["captured"])
            self.assertEqual(result["error"], "identity_changed")
            self.assertIn(
                "app_or_profile_verification_failed", result["cleanup_failures"]
            )
            self.assertFalse(device.frozen)

    def test_capture_refuses_overwrite_and_skipped_slots(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            with self.assertRaises(FileNotFoundError):
                tool.capture(directory, "02-B", device, device.clock.sleep)
            run_capture(directory, device)
            before = list(device.events)
            with self.assertRaises(FileExistsError):
                run_capture(directory, device)
            self.assertEqual(before, device.events)

    def test_workload_or_configuration_tamper_prevents_device_access(self):
        for name in ("workload.json", "trace.pbtxt"):
            with tempfile.TemporaryDirectory() as root:
                directory = initialize(Path(root))
                file = directory / name
                file.write_bytes(file.read_bytes() + b" ")
                device = FakeDevice(Clock())
                with (
                    self.subTest(name=name),
                    self.assertRaises(tool.compare.InvalidWindow),
                ):
                    run_capture(directory, device)
                self.assertEqual(device.events, [])


class WakeProbeTests(unittest.TestCase):
    def test_real_probe_arguments_keep_legacy_diagnostics_unchanged(self):
        device = tool.Device("PIXEL1234")
        raw = b"3 packets transmitted, 2 received, 33% packet loss\n"
        for inbound in (True, False):
            with (
                self.subTest(inbound=inbound),
                patch.object(tool, "bounded_run", return_value=(0, raw)) as run,
            ):
                result = device.probe(workload(), inbound)
            ping = ["ping", "-n", "-c", "3", "-W", "3", "-I"]
            ping += (
                ["10.42.0.1", "10.42.0.2"] if inbound else ["10.42.0.2", "10.42.0.1"]
            )
            run.assert_called_once_with(
                ping
                if inbound
                else ["adb", "-s", "PIXEL1234", "shell", shlex.join(ping)],
                timeout=15,
                check=False,
            )
            self.assertEqual(
                result,
                {"success": True, "exit_code": 0, "summary": [raw.decode().strip()]},
            )

    def test_real_wake_arguments_allow_delayed_first_packet_then_require_steady(self):
        device = tool.Device("PIXEL1234")
        for inbound in (True, False):
            for received in ("received", "packets received"):
                cold = f"1 packets transmitted, 1 {received}, 0% packet loss\n".encode()
                steady = (
                    f"3 packets transmitted, 3 {received}, 0% packet loss\n".encode()
                )
                with (
                    self.subTest(inbound=inbound, received=received),
                    patch.object(tool.time, "monotonic", side_effect=[10, 54, 54, 56]),
                    patch.object(
                        tool, "bounded_run", side_effect=[(0, cold), (0, steady)]
                    ) as run,
                ):
                    result = device.wake_probe(workload(), inbound)
                self.assertTrue(result["success"])
                self.assertEqual(result["cold"]["elapsed_seconds"], 44)
                self.assertEqual(result["cold"]["stdout"], cold.decode())
                self.assertEqual(result["steady"]["elapsed_seconds"], 2)
                self.assertEqual(run.call_count, 2)
                for call, count, wait, timeout in zip(
                    run.call_args_list, (1, 3), (45, 3), (50, 15)
                ):
                    ping = ["ping", "-n", "-c", str(count), "-W", str(wait), "-I"]
                    ping += (
                        ["10.42.0.1", "10.42.0.2"]
                        if inbound
                        else ["10.42.0.2", "10.42.0.1"]
                    )
                    self.assertEqual(
                        call.args,
                        (
                            ping
                            if inbound
                            else ["adb", "-s", "PIXEL1234", "shell", shlex.join(ping)],
                        ),
                    )
                    self.assertEqual(call.kwargs, {"timeout": timeout, "check": False})

    def test_failed_cold_cannot_be_rescued_by_queued_steady_success(self):
        failed = b"1 packets transmitted, 0 received, 100% packet loss\n"
        steady = b"3 packets transmitted, 3 received, 0% packet loss\n"
        for inbound in (True, False):
            with (
                self.subTest(inbound=inbound),
                patch.object(tool.time, "monotonic", side_effect=[0, 45]),
                patch.object(
                    tool, "bounded_run", side_effect=[(1, failed), (0, steady)]
                ) as run,
            ):
                result = tool.Device("PIXEL1234").wake_probe(workload(), inbound)
            self.assertFalse(result["success"])
            self.assertEqual(result["cold"]["stdout"], failed.decode())
            self.assertEqual(result["cold"]["elapsed_seconds"], 45)
            self.assertEqual(result["cold"]["exit_code"], 1)
            self.assertEqual(result["cold"]["error"], "packet_delivery_failed")
            self.assertIsNone(result["steady"])
            self.assertEqual(run.call_count, 1)

    def test_zero_exit_still_requires_one_of_one_then_three_of_three(self):
        cold = b"1 packets transmitted, 1 received, 0% packet loss\n"
        for responses in (
            [(0, b"1 packets transmitted, 0 received, 100% packet loss\n")],
            [(0, b"missing summary\n")],
            [(0, cold + cold)],
            [(0, cold), (0, b"3 packets transmitted, 2 received, 33% packet loss\n")],
            [(0, cold), (0, b"3 packets transmitted, 0 received, 100% packet loss\n")],
        ):
            with (
                self.subTest(responses=responses),
                patch.object(tool, "bounded_run", side_effect=responses) as run,
            ):
                result = tool.Device("PIXEL1234").wake_probe(workload(), True)
            self.assertFalse(result["success"])
            self.assertEqual(run.call_count, len(responses))

    def test_command_failures_preserve_stage_stdout_timing_and_never_retry(self):
        cold = b"1 packets transmitted, 1 received, 0% packet loss\n"
        for inbound in (True, False):
            for stage in ("cold", "steady"):
                for code in ("command_timeout", "command_interrupted"):
                    error = tool.CommandFailure(
                        code, b"partial\xff", b"private-stderr", None
                    )
                    responses = [error] if stage == "cold" else [(0, cold), error]
                    with (
                        self.subTest(inbound=inbound, stage=stage, code=code),
                        patch.object(
                            tool.time, "monotonic", side_effect=[0, 50, 50, 65]
                        ),
                        patch.object(tool, "bounded_run", side_effect=responses) as run,
                    ):
                        result = tool.Device("PIXEL1234").wake_probe(
                            workload(), inbound
                        )
                    self.assertFalse(result["success"])
                    self.assertEqual(result["error"], code)
                    self.assertEqual(result[stage]["error"], code)
                    self.assertEqual(
                        base64.b64decode(result[stage]["stdout_base64"]), error.stdout
                    )
                    self.assertEqual(
                        base64.b64decode(result[stage]["stderr_base64"]), error.stderr
                    )
                    self.assertEqual(
                        result[stage]["elapsed_seconds"], 50 if stage == "cold" else 15
                    )
                    self.assertEqual(run.call_count, len(responses))
                    if stage == "cold":
                        self.assertIsNone(result["steady"])
                    else:
                        self.assertTrue(result["cold"]["success"])
                        self.assertEqual(result["cold"]["stdout"], cold.decode())


class DeviceSafetyTests(unittest.TestCase):
    def test_network_dump_filters_current_agents_and_preserves_failures(self):
        device = tool.Device("PIXEL1234")
        raw = "Active default network: 123\n  NetworkAgentInfo{network{123} ni{MOBILE[LTE] IS_VALIDATED SubscriptionIds: {1}}"
        with patch.object(device, "shell", return_value=raw) as shell:
            self.assertEqual(
                tool.subscription_id(tool.validated_cellular(device.network_state())),
                1,
            )
        shell.assert_called_once_with(
            "sh",
            "-c",
            "set -o pipefail; dumpsys connectivity | grep -E "
            "'^[[:space:]]*(Active default network:|NetworkAgentInfo[{]network[{])'",
        )
        with (
            patch.object(
                device,
                "shell",
                side_effect=tool.CommandFailure("command_failed", b"", b"", 1),
            ),
            self.assertRaises(tool.CommandFailure),
        ):
            device.network_state()

    def test_radio_dump_filters_history_on_device_and_preserves_failures(self):
        device = tool.Device("PIXEL1234")
        with patch.object(device, "shell", return_value=radio_state()) as shell:
            self.assertEqual(
                tool.parse_signal(device.radio_state(), 1)["rsrp_dbm"], -85
            )
        shell.assert_called_once_with(
            "sh",
            "-c",
            "set -o pipefail; dumpsys telephony.registry "
            "| sed -n '/last known state:/,/local logs:/p'",
        )
        failure = tool.CommandFailure("command_failed", b"", b"unavailable", 1)
        with (
            patch.object(device, "shell", side_effect=failure),
            self.assertRaises(tool.CommandFailure),
        ):
            device.radio_state()

    def test_subscription_binding_rejects_ambiguous_capabilities(self):
        self.assertEqual(
            tool.subscription_id("SubscriptionIds: {1} Specifier: mSubId=1"), 1
        )
        self.assertEqual(tool.subscription_id("SubscriptionIds: [1]"), 1)
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.subscription_id("SubscriptionIds: {1,2} Specifier: mSubId=1")

    def test_watchdog_script_restores_without_the_host_and_can_be_disarmed(self):
        class LocalDevice(tool.Device):
            def __init__(self, root):
                super().__init__("LOCALTEST1234")
                self.root = root

            def translate(self, value):
                return value.replace("/data/local/tmp", str(self.root))

            def shell(self, *args, check=True):
                if args == ("dumpsys", "battery"):
                    return (
                        "USB powered: true"
                        if (self.root / "state").read_text() == "hardware"
                        else "UPDATES STOPPED\nUSB powered: false"
                    )
                if args == ("dumpsys", "battery", "unplug", "-f"):
                    (self.root / "state").write_text("simulated")
                    return ""
                values = [self.translate(value) for value in args]
                return tool.bounded_run(values, check=check)[1].decode().strip()

            def adb(self, *args, **kwargs):
                if args[0] != "shell":
                    raise AssertionError("local harness never invokes ADB")
                return tool.bounded_run(["sh", "-c", self.translate(args[1])], **kwargs)

        for automatic in (True, False):
            with tempfile.TemporaryDirectory() as root:
                root = Path(root)
                (root / "state").write_text("hardware")
                (root / "dumpsys").write_text(
                    f"#!{sys.executable}\nimport os,pathlib\np=pathlib.Path(os.environ['WATCHDOG_TEST_ROOT'])\n(p/'state').write_text('hardware')\nwith (p/'resets').open('a') as f: f.write('reset\\n')\n"
                )
                (root / "dumpsys").chmod(0o700)
                dev = LocalDevice(root)
                tag = "p2p-vpn-battery-" + uuid.uuid4().hex
                pid = None
                with patch.dict(
                    os.environ,
                    {
                        "PATH": str(root) + os.pathsep + os.environ["PATH"],
                        "WATCHDOG_TEST_ROOT": str(root),
                    },
                ):
                    try:
                        pid = dev.fallback(tag, 3)
                        self.assertTrue((root / (tag + ".ready")).exists())
                        dev.begin_override(pid, tag)
                        self.assertEqual((root / "state").read_text(), "simulated")
                        if automatic:
                            deadline = time.monotonic() + 8
                            while (
                                time.monotonic() < deadline
                                and (root / "state").read_text() != "hardware"
                            ):
                                time.sleep(0.05)
                            self.assertEqual((root / "state").read_text(), "hardware")
                            dev.stop_owned(pid, tag, "-TERM")
                        else:
                            dev.restore(pid, tag)
                        self.assertIsNone(dev.process_args(pid))
                        self.assertEqual((root / "resets").read_text(), "reset\n")
                        self.assertFalse((root / (tag + ".armed")).exists())
                        self.assertFalse((root / (tag + ".ready")).exists())
                    finally:
                        if pid:
                            dev.stop_owned(pid, tag, "-TERM")

    def test_actual_endpoint_retains_report_and_boot_brackets(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            fake = FakeDevice(Clock())
            fake.endpoint(root, "fixture", None, 10304)
            raw = (root / "fixture.pb").read_bytes()
            device = tool.Device("PIXEL1234")
            boot = "a1234567-89ab-4cde-8f01-234567890abc"
            with (
                patch.object(
                    device, "anchor", side_effect=[(boot, 10000000), (boot, 10000010)]
                ),
                patch.object(device, "adb", return_value=(0, raw)),
            ):
                result = device.endpoint(root, "start", {"test": True}, 10304)
            self.assertEqual((root / "start.pb").read_bytes(), raw)
            self.assertEqual(result["boot_id"], boot)
            self.assertEqual(result["after_uptime_ms"], 10000010)
            self.assertTrue((root / "start-clock.json").exists())
            with (
                patch.object(
                    device, "anchor", side_effect=[(boot, 10000000), (boot, 10000010)]
                ),
                patch.object(device, "adb", return_value=(0, b"invalid-report")),
                self.assertRaises(
                    (
                        tool.compare.decoder.InvalidReport,
                        tool.compare.decoder.DecodeError,
                    )
                ),
            ):
                device.endpoint(root, "bad", None, 10304)
            self.assertEqual((root / "bad.pb").read_bytes(), b"invalid-report")

    def test_anchor_uses_monotonic_clock_not_wall_time(self):
        device = tool.Device("PIXEL1234")
        raw = "a1234567-89ab-4cde-8f01-234567890abc\n196348.37 12345.00"
        with patch.object(device, "shell", return_value=raw):
            self.assertEqual(device.anchor()[1], 196348370)
        with (
            patch.object(device, "shell", return_value=raw.replace("196348.37", "nan")),
            self.assertRaises(tool.compare.InvalidWindow),
        ):
            device.anchor()

    def test_trace_copied_before_only_owned_remote_files_are_removed(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            device = tool.Device("PIXEL1234")
            with (
                patch.object(device, "stop_owned") as stop,
                patch.object(device, "shell", side_effect=["5", ""]) as shell,
                patch.object(device, "adb", return_value=(0, b"TRACE")),
            ):
                device.retain_trace(
                    root, 456, "/owned-config", "/owned-trace", False, lambda _: None
                )
            stop.assert_called_once_with(456, "/owned-config", "-INT")
            self.assertEqual((root / "trace.pftrace").read_bytes(), b"TRACE")
            self.assertEqual(
                shell.call_args.args, ("rm", "--", "/owned-config", "/owned-trace")
            )

    def test_active_trace_is_not_read_or_deleted(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(
                device,
                "stop_owned",
                side_effect=tool.compare.InvalidWindow("process_stop_unverified"),
            ),
            patch.object(device, "shell") as shell,
            patch.object(device, "adb") as adb,
            self.assertRaisesRegex(
                tool.compare.InvalidWindow, "process_stop_unverified"
            ),
        ):
            device.retain_trace(
                Path("unused"),
                456,
                "/owned-config",
                "/owned-trace",
                False,
                lambda _: None,
            )
        adb.assert_not_called()
        shell.assert_not_called()

    def test_preflight_rejects_non_tun_route_before_mutations(self):
        device = tool.Device("PIXEL1234")
        responses = [
            "mWakefulness=Asleep\nmHalInteractiveModeEnabled=false",
            "Active default network: 100\nNetworkAgentInfo{network{100} ni{MOBILE[] IS_VALIDATED SubscriptionIds: {1}",
            radio_state(),
            THERMAL,
            "0",
            "USB powered: true",
            "",
        ]
        for kind in ("tun", "veth"):
            with (
                patch.object(device, "usb"),
                patch.object(device, "shell", side_effect=responses),
                patch.object(
                    tool,
                    "bounded_run",
                    side_effect=[
                        (0, b'[{"dev":"pv0"}]'),
                        (0, json.dumps([{"linkinfo": {"info_kind": kind}}]).encode()),
                    ],
                ),
            ):
                if kind == "tun":
                    device.preflight(workload())
                else:
                    with self.assertRaisesRegex(
                        tool.compare.InvalidWindow, "overlay_route"
                    ):
                        device.preflight(workload())

    def test_identity_does_not_equate_live_sessions_with_authorization(self):
        device = tool.Device("PIXEL1234")
        network_id = "a1234567-89ab-4cde-8f01-234567890abc"
        resources = {
            "networks": [
                {
                    "id": network_id,
                    "phase": "running",
                    "counters": [
                        "outbound_sent_packets 10",
                        "inbound_accepted_packets 12",
                    ],
                }
            ]
        }
        snapshot = {
            "connected": True,
            "busy": False,
            "pairing": {"code": None, "candidate_peer": None},
            "networks": [
                {
                    "id": network_id,
                    "name": "battery-trial",
                    "peer_id": workload()["phone_peer_id"],
                    "addresses": ["10.42.0.2/32", "fd42::2/128"],
                    "enabled": True,
                }
            ],
            "underlay": {},
            "paths": {"connected_peers": 0},
            "runtime_generation": 1,
        }
        with (
            patch.object(device, "usb"),
            patch.object(
                device,
                "rpc",
                side_effect=[resources, {"service_ready": True, "snapshot": snapshot}],
            ),
            patch.object(
                device,
                "shell",
                side_effect=["package:/data/app/test/base.apk", "android-test-build"],
            ),
            patch.object(device, "file_hash", return_value="a" * 64),
        ):
            identity, evidence = device.identities(workload())
        self.assertEqual(evidence["paths"]["connected_peers"], 0)
        self.assertEqual(
            identity["peer_inventory"],
            tool.sha(json.dumps(sorted(workload()["authorized_peer_ids"])).encode()),
        )
        snapshot["pairing"]["code"] = "private-pairing-code"
        with (
            patch.object(device, "usb"),
            patch.object(
                device,
                "rpc",
                side_effect=[resources, {"service_ready": True, "snapshot": snapshot}],
            ),
            self.assertRaisesRegex(tool.compare.InvalidWindow, "pairing_active"),
        ):
            device.identities(workload())

    def test_serial_cannot_select_wifi_adb_or_inject_options(self):
        for serial in ("192.168.1.2:5555", "-s1234", "phone name", "", "x" * 65):
            with (
                self.subTest(serial=serial),
                self.assertRaises(tool.compare.InvalidWindow),
            ):
                tool.Device(serial)

    def test_usb_transport_is_required(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "adb", return_value=(0, b"192.168.1.2:5555")),
            self.assertRaises(tool.compare.InvalidWindow),
        ):
            device.usb()

    def test_no_unrelated_process_can_be_killed(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "process_args", return_value=["unrelated", "process"]),
            patch.object(device, "shell") as shell,
        ):
            with self.assertRaisesRegex(
                tool.compare.InvalidWindow, "process_ownership"
            ):
                device.stop_owned(321, "our-tag", "-TERM")
            shell.assert_not_called()

    def test_existing_battery_override_is_not_reset(self):
        device = tool.Device("PIXEL1234")
        with patch.object(
            device, "shell", return_value="UPDATES STOPPED\nUSB powered: false"
        ) as shell:
            with self.assertRaises(tool.compare.InvalidWindow):
                device.fallback("our-tag", 2460)
            self.assertEqual(shell.call_count, 1)

    def test_fallback_is_not_canceled_until_reporting_is_verified(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "shell", side_effect=["", "UPDATES STOPPED"]),
            patch.object(device, "stop_owned") as stop,
        ):
            with self.assertRaisesRegex(
                tool.compare.InvalidWindow, "restoration_unverified"
            ):
                device.restore(321, "our-tag")
            stop.assert_not_called()

    def test_only_tagged_fallback_and_no_history_reset_commands(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(
                device, "shell", side_effect=["USB powered: true", "321", "321:322"]
            ) as shell,
            patch.object(
                device,
                "process_args",
                side_effect=[["sh", "-c", "script", "our-tag"], ["sleep", "2460"]],
            ),
        ):
            self.assertEqual(device.fallback("our-tag", 2460), 321)
            command = shell.call_args_list[1].args[-1]
            self.assertIn("our-tag", command)
            self.assertIn("sleep 2460", command)
            self.assertIn("dumpsys battery reset -f", command)
            self.assertNotIn("batterystats --reset", command)

    def test_fallback_rejects_invalid_pid(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(
                device, "shell", side_effect=["USB powered: true", "not-a-pid"]
            ),
            self.assertRaises(tool.compare.InvalidWindow),
        ):
            device.fallback("our-tag", 2460)

    def test_ambiguous_radio_is_not_arbitrarily_selected(self):
        self.assertEqual(tool.parse_signal(radio_state(), 1)["rsrp_dbm"], -85)
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.parse_signal(radio_state(first="rsrp=-85 ssRsrp=-95"), 1)

    def test_other_sim_cannot_supply_unavailable_serving_signal(self):
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.parse_signal(
                radio_state(first="rsrp=2147483647", second="rsrp=-85"), 1
            )
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.parse_signal(
                radio_state().replace("mActiveDataSubId=1", "mActiveDataSubId=2"), 1
            )

    def test_unreadable_process_is_not_classified_as_absent(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "adb", side_effect=[(1, b""), (0, b"")]),
            patch.object(device, "shell") as shell,
        ):
            with self.assertRaisesRegex(
                tool.compare.InvalidWindow, "process_inspection_unavailable"
            ):
                device.stop_owned(321, "our-tag", "-TERM")
            shell.assert_not_called()

    def test_adb_failure_cannot_masquerade_as_missing_process(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "adb", side_effect=[(1, b""), (1, b"")]),
            self.assertRaisesRegex(
                tool.compare.InvalidWindow, "process_inspection_unavailable"
            ),
        ):
            device.process_args(321)

    def test_stop_requires_verified_exit(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "process_args", return_value=["sh", "our-tag"]),
            patch.object(device, "shell") as shell,
            patch.object(tool.time, "sleep"),
            self.assertRaisesRegex(
                tool.compare.InvalidWindow, "process_stop_unverified"
            ),
        ):
            device.stop_owned(321, "our-tag", "-TERM")
        shell.assert_called_once_with("kill", "-TERM", "321")

    def test_missing_watchdog_child_prevents_acknowledgement(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(
                device, "shell", side_effect=["USB powered: true", "321", "321:322"]
            ),
            patch.object(device, "process_args", side_effect=[["sh", "our-tag"], None]),
            patch.object(device, "stop_owned") as stop,
            self.assertRaisesRegex(tool.compare.InvalidWindow, "fallback_not_ready"),
        ):
            device.fallback("our-tag", 2460)
        stop.assert_called_once_with(321, "our-tag", "-TERM")

    def test_expired_watchdog_cannot_arm_an_override(self):
        device = tool.Device("PIXEL1234")
        with (
            patch.object(device, "process_args", return_value=None),
            patch.object(device, "shell") as shell,
            self.assertRaisesRegex(tool.compare.InvalidWindow, "fallback_not_ready"),
        ):
            device.begin_override(321, "our-tag")
        shell.assert_not_called()

    def test_failed_battery_command_keeps_partial_stdout_and_stderr(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            device = tool.Device("PIXEL1234")
            boot = "a1234567-89ab-4cde-8f01-234567890abc"
            error = tool.CommandFailure(
                "command_timeout", b"partial-protobuf", b"private-stderr", None
            )
            with (
                patch.object(
                    device, "anchor", side_effect=[(boot, 10000000), (boot, 10000010)]
                ),
                patch.object(device, "adb", side_effect=error),
                self.assertRaises(tool.CommandFailure),
            ):
                device.endpoint(root, "start", None, 10304)
            self.assertEqual((root / "start.pb").read_bytes(), b"partial-protobuf")
            self.assertEqual(
                (root / "start-command-stderr.txt").read_bytes(), b"private-stderr"
            )

    def test_command_timeout_retains_partial_output_without_live_child(self):
        with self.assertRaises(tool.CommandFailure) as raised:
            tool.bounded_run(
                [
                    sys.executable,
                    "-c",
                    "import sys,time;print('partial',flush=True);sys.stderr.write('error\\n');sys.stderr.flush();time.sleep(10)",
                ],
                timeout=0.5,
            )
        self.assertEqual(raised.exception.stdout, b"partial\n")
        self.assertEqual(raised.exception.stderr, b"error\n")
        self.assertEqual(str(raised.exception), "command_timeout")

    def test_temperature_uses_live_hal_only(self):
        raw = "Thermal Status: 0\nCurrent temperatures from HAL:\nTemperature{mValue=28.1, mType=2, mName=battery, foo}\nCurrent cooling devices"
        self.assertEqual(tool.live_temperature(raw), 281)
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.live_temperature(raw.replace("Thermal Status: 0", "Thermal Status: 1"))
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.live_temperature("temperature: 281")

    def test_screen_off_includes_ambient_doze_but_not_interactive(self):
        for state in ("Asleep", "Dozing"):
            tool.screen_off(f"mWakefulness={state}\nmHalInteractiveModeEnabled=false")
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.screen_off("mWakefulness=Awake\nmHalInteractiveModeEnabled=true")

    def test_default_cellular_is_required_not_any_mobile_network(self):
        raw = "Active default network: 100\nNetworkAgentInfo{network{100} ni{MOBILE[] IS_VALIDATED"
        tool.validated_cellular(raw)
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.validated_cellular(raw.replace("network{100}", "network{101}"))


class PlanAndCliTests(unittest.TestCase):
    def test_old_collector_plan_is_rejected_without_touching_retained_failure(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            device = FakeDevice(Clock())
            device.fail_after_probe = True
            run_capture(directory, device)
            retained = {
                file.name: file.read_bytes() for file in (directory / "01-A").iterdir()
            }
            plan = tool.compare.read_json(directory / "plan.json")
            plan["collector_source_sha256"] = "0" * 64
            raw = json.dumps(plan).encode()
            (directory / "plan.json").write_bytes(raw)
            device = FakeDevice(Clock())
            with self.assertRaisesRegex(
                tool.compare.InvalidWindow, "collector_changed"
            ):
                run_capture(directory, device, "02-B")
            self.assertEqual(device.events, [])
            self.assertFalse((directory / "02-B").exists())
            self.assertEqual((directory / "plan.json").read_bytes(), raw)
            self.assertEqual(
                {
                    file.name: file.read_bytes()
                    for file in (directory / "01-A").iterdir()
                },
                retained,
            )

    def test_host_watchdog_includes_all_four_bounded_boundary_checks(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            with (
                patch.object(
                    sys,
                    "argv",
                    [
                        str(path),
                        "capture",
                        str(directory),
                        "--slot",
                        "01-A",
                        "--serial",
                        "PIXEL1234",
                        "--simulate-on-battery",
                    ],
                ),
                patch.object(tool, "Device"),
                patch.object(
                    tool, "device_lock", return_value=contextlib.nullcontext()
                ),
                patch.object(
                    tool,
                    "capture",
                    return_value={
                        "captured": True,
                        "restoration_verified": True,
                        "boundary_probes_passed": True,
                        "cleanup_failures": [],
                    },
                ),
                patch.object(tool.signal, "signal"),
                patch.object(tool.signal, "alarm") as alarm,
            ):
                self.assertEqual(tool.main(), 0)
            self.assertEqual(
                [call.args for call in alarm.call_args_list],
                [(300 + 1800 + 740,), (0,)],
            )
            self.assertEqual(
                tool.WATCHDOG_ALLOWANCE_SECONDS,
                480
                + 4
                * (tool.WAKE_PROBE_TIMEOUT_SECONDS + tool.STEADY_PROBE_TIMEOUT_SECONDS),
            )

    def test_cli_initializes_frozen_series_without_device_access(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            initialize(root)
            directory = root / "cli-series"
            result = subprocess.run(
                [
                    sys.executable,
                    str(path),
                    "init",
                    str(directory),
                    "--reference-apk",
                    str(root / "reference.apk"),
                    "--candidate-apk",
                    str(root / "candidate.apk"),
                    "--workload",
                    str(root / "workload.json"),
                ],
                capture_output=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(json.loads(result.stdout)["stage"], "initialized")
            self.assertEqual(
                tool.compare.read_json(directory / "plan.json")["workload_sha256"],
                tool.sha((root / "workload.json").read_bytes()),
            )

    def test_frozen_plan_and_permissions(self):
        with tempfile.TemporaryDirectory() as root:
            directory = initialize(Path(root))
            plan = tool.compare.read_json(directory / "plan.json")
            self.assertEqual(plan["window_seconds"], 1800)
            self.assertEqual(plan["settle_seconds"], 300)
            self.assertEqual(plan["reference_apk"], tool.sha(b"reference-test-only"))
            self.assertEqual(
                plan["collector_source_sha256"], tool.sha(path.read_bytes())
            )
            self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            for file in directory.iterdir():
                self.assertEqual(file.stat().st_mode & 0o777, 0o600)
            self.assertIn(
                b"duration_ms: 1980000", (directory / "trace.pbtxt").read_bytes()
            )
            config = (directory / "trace.pbtxt").read_bytes()
            self.assertIn(b"fill_policy: RING_BUFFER", config)
            self.assertIn(b"write_into_file: true", config)
            self.assertIn(b"file_write_period_ms: 5000", config)
            self.assertIn(f"max_file_size_bytes: {tool.MAX_TRACE}".encode(), config)

    def test_workload_rejects_overrides_duplicate_peers_and_commands(self):
        for change in ("override", "duplicate", "secret", "command", "address"):
            value = copy.deepcopy(workload())
            if change == "override":
                value["diagnostic_overrides"] = True
            elif change == "duplicate":
                value["authorized_peer_ids"] *= 2
            elif change == "secret":
                value["private_key"] = "never accepted"
            elif change == "command":
                value["network_name"] = "x; command"
            else:
                value["phone_address"] = "127.0.0.1"
            with (
                self.subTest(change=change),
                self.assertRaises(tool.compare.InvalidWindow),
            ):
                tool.validate_workload(value)

    def test_device_lock_is_real_and_exclusive(self):
        with (
            tempfile.TemporaryDirectory() as root,
            patch.object(tool.Path, "home", return_value=Path(root)),
        ):
            with (
                tool.device_lock("PIXEL1234"),
                self.assertRaisesRegex(tool.compare.InvalidWindow, "device_busy"),
                tool.device_lock("PIXEL1234"),
            ):
                self.fail("second lock must fail")
            with tool.device_lock("PIXEL1234"):
                pass

    def test_bounded_child_output(self):
        with self.assertRaises(tool.compare.InvalidWindow):
            tool.bounded_run([sys.executable, "-c", "print('x' * 100000)"], limit=1024)

    def test_cli_wireless_serial_failure_is_redacted_and_no_device_call(self):
        result = subprocess.run(
            [
                sys.executable,
                str(path),
                "capture",
                "/private-missing-test-path",
                "--slot",
                "01-A",
                "--serial",
                "192.168.1.2:5555",
                "--simulate-on-battery",
            ],
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(json.loads(result.stdout)["error"], "usb_serial")
        self.assertNotIn(b"Traceback", result.stderr)


unittest.main()

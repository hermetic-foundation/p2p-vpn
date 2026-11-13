import importlib.util
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import unittest

path = Path(
    os.environ.get(
        "P2P_VPN_BATTERY_DECODER",
        Path(__file__).parent.parent / "scripts/android-battery-decode.py",
    )
)
spec = importlib.util.spec_from_file_location("decoder", path)
decoder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decoder)


def fixture():
    value = decoder.report_type()()
    stats = value.batterystats
    stats.report_version = 36
    stats.parcel_version = 216
    stats.uids.add(uid=10303).power_use_item.computed_power_mah = 12.399919123456789
    battery = stats.system.battery
    for field in decoder.SCOPE_FIELDS:
        setattr(battery, field, 123)
    return value


class BatteryDecoderTests(unittest.TestCase):
    def test_golden_aosp_wire_tags(self):
        raw = bytes.fromhex(
            "0a31082410d8012a0f08bf5092010909000000000000294032190a17"
            "08e807100118904e20a84628882730a01f38a01f40b817"
        )
        value = decoder.decode(raw, 10303)
        self.assertEqual(value["uid_consumption_mah"], 12.5)
        self.assertEqual(value["scope"]["start_clock_time_ms"], 1000)
        self.assertEqual(value["scope"]["screen_off_realtime_ms"], 4000)

    def test_double_precision(self):
        value = decoder.decode(fixture().SerializeToString(), 10303)
        self.assertEqual(value["uid_consumption_mah"], 12.399919123456789)

    def test_other_uids_not_exposed(self):
        message = fixture()
        expected = decoder.decode(message.SerializeToString(), 10303)
        message.batterystats.uids.add(uid=12345).power_use_item.computed_power_mah = 9999
        value = decoder.decode(message.SerializeToString(), 10303)
        self.assertEqual(value.keys(), expected.keys())
        for key in ("uid", "uid_consumption_mah", "uid_screen_consumption_mah", "scope"):
            self.assertEqual(value[key], expected[key])
        self.assertNotIn("uids", value)
        self.assertNotIn("packages", value)

    def test_zero_is_available(self):
        message = fixture()
        message.batterystats.uids[0].power_use_item.computed_power_mah = 0
        value = decoder.decode(message.SerializeToString(), 10303)
        self.assertTrue(value["uid_attribution_available"])
        self.assertEqual(value["uid_consumption_mah"], 0)

    def test_absent_uid_and_power_are_unavailable_not_zero(self):
        message = fixture()
        for uid in (10304, 10303):
            message.batterystats.uids[0].ClearField("power_use_item")
            value = decoder.decode(message.SerializeToString(), uid)
            self.assertFalse(value["uid_attribution_available"])
            self.assertIsNone(value["uid_consumption_mah"])
            self.assertIsNone(value["uid_screen_consumption_mah"])

    def test_duplicate_uid(self):
        message = fixture()
        message.batterystats.uids.add(uid=10303)
        with self.assertRaises(decoder.InvalidReport):
            decoder.decode(message.SerializeToString(), 10303)

    def test_invalid_power(self):
        for power in (-1, math.nan, math.inf):
            message = fixture()
            message.batterystats.uids[0].power_use_item.computed_power_mah = power
            with self.subTest(power=power), self.assertRaises(decoder.InvalidReport):
                decoder.decode(message.SerializeToString(), 10303)

    def test_missing_clocks(self):
        for field in decoder.SCOPE_FIELDS:
            if field == "start_count":
                continue
            message = fixture()
            message.batterystats.system.battery.ClearField(field)
            with self.subTest(field=field), self.assertRaises(decoder.InvalidReport):
                decoder.decode(message.SerializeToString(), 10303)

    def test_android_omitted_zero_start_count(self):
        message = fixture()
        message.batterystats.system.battery.ClearField("start_count")
        value = decoder.decode(message.SerializeToString(), 10303)
        self.assertEqual(value["scope"]["start_count"], 0)

    def test_retained_pixel_clock_projection(self):
        message = fixture()
        battery = message.batterystats.system.battery
        battery.ClearField("start_count")
        for field, value in {
            "start_clock_time_ms": 1794453312970,
            "total_realtime_ms": 915606, "total_uptime_ms": 915607,
            "battery_realtime_ms": 915606, "battery_uptime_ms": 915607,
            "screen_off_realtime_ms": 910480, "screen_off_uptime_ms": 910482,
        }.items():
            setattr(battery, field, value)
        value = decoder.decode(message.SerializeToString(), 10303)
        self.assertEqual(value["scope"]["screen_off_uptime_ms"], 910482)

    def test_cross_clock_skew_is_bounded_and_same_clock_order_stays_strict(self):
        for prefix in ("total", "battery", "screen_off"):
            message = fixture()
            battery = message.batterystats.system.battery
            for field in ("total_uptime_ms", "battery_uptime_ms", "screen_off_uptime_ms"):
                setattr(battery, field, 123 + decoder.CLOCK_SKEW_MS)
            decoder.decode(message.SerializeToString(), 10303)
            setattr(battery, prefix + "_uptime_ms", 124 + decoder.CLOCK_SKEW_MS)
            with self.subTest(prefix=prefix), self.assertRaises(decoder.InvalidReport):
                decoder.decode(message.SerializeToString(), 10303)

    def test_invalid_clock_order(self):
        message = fixture()
        message.batterystats.system.battery.screen_off_realtime_ms = 124
        with self.assertRaises(decoder.InvalidReport):
            decoder.decode(message.SerializeToString(), 10303)

    def test_schema_version(self):
        message = fixture()
        message.batterystats.parcel_version = 217
        with self.assertRaises(decoder.InvalidReport):
            decoder.decode(message.SerializeToString(), 10303)

    def test_input_bounds(self):
        for raw in (b"", b"x" * (decoder.MAX_REPORT_BYTES + 1)):
            with self.assertRaises(decoder.InvalidReport):
                decoder.decode(raw, 10303)

    def test_no_physical_or_window_claim(self):
        value = decoder.decode(fixture().SerializeToString(), 10303)
        self.assertTrue(value["single_endpoint_only"])
        self.assertFalse(value["physical_energy_measurement"])

    def test_cli_preserves_precision(self):
        result = subprocess.run(
            [sys.executable, str(path), "--uid", "10303"],
            input=fixture().SerializeToString(),
            capture_output=True,
            check=True,
        )
        self.assertEqual(
            json.loads(result.stdout)["uid_consumption_mah"],
            fixture().batterystats.uids[0].power_use_item.computed_power_mah,
        )

    def test_cli_redacts_failures(self):
        raw = b"private-not-a-battery-report"
        result = subprocess.run(
            [sys.executable, str(path), "--uid", "10303"],
            input=raw,
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(raw, result.stderr)
        self.assertNotIn(b"Traceback", result.stderr)
        self.assertEqual(result.stdout, b"")


unittest.main()

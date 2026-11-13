"""Decode one bounded Android UID battery estimate without display rounding."""

import argparse
import functools
import hashlib
import json
import math
from pathlib import Path
import signal
import subprocess
import sys

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory
from google.protobuf.message import DecodeError

MAX_REPORT_BYTES = 2 * 1024 * 1024
CLOCK_SKEW_MS = 5
SCHEMA = Path(__file__).with_suffix(".proto")
SCOPE_FIELDS = (
    "start_clock_time_ms",
    "start_count",
    "total_realtime_ms",
    "total_uptime_ms",
    "battery_realtime_ms",
    "battery_uptime_ms",
    "screen_off_realtime_ms",
    "screen_off_uptime_ms",
)


class InvalidReport(ValueError):
    """A report is missing, ambiguous, unsupported or invalid."""


@functools.lru_cache(maxsize=1)
def report_type():
    result = subprocess.run(
        ["protoc", "-I", str(SCHEMA.parent), "--descriptor_set_out=/dev/stdout", SCHEMA.name],
        check=True,
        capture_output=True,
        timeout=10,
    )
    if len(result.stdout) > 16384:
        raise InvalidReport("schema size")
    descriptor = descriptor_pb2.FileDescriptorSet.FromString(result.stdout)
    if len(descriptor.file) != 1:
        raise InvalidReport("schema count")
    pool = descriptor_pool.DescriptorPool()
    pool.Add(descriptor.file[0])
    descriptor = pool.FindMessageTypeByName("p2pvpn.battery.ServiceDump")
    return message_factory.GetMessageClass(descriptor)


def optional_charge(message, field):
    if not message.HasField(field):
        return None
    value = getattr(message, field)
    if not math.isfinite(value) or value < 0:
        raise InvalidReport("charge")
    return value


def decode(raw, uid):
    if not 0 < len(raw) <= MAX_REPORT_BYTES or not 0 <= uid < 2**31:
        raise InvalidReport("input bounds")
    message = report_type().FromString(raw)
    if not message.HasField("batterystats"):
        raise InvalidReport("missing statistics")
    stats = message.batterystats
    if stats.report_version != 36 or stats.parcel_version != 216:
        raise InvalidReport("unsupported Android schema")
    if not stats.HasField("system") or not stats.system.HasField("battery"):
        raise InvalidReport("missing scope")
    battery = stats.system.battery
    # Android's ProtoOutputStream omits a zero-valued START_COUNT.
    if not all(battery.HasField(field) for field in SCOPE_FIELDS if field != "start_count"):
        raise InvalidReport("missing clock")
    scope = {field: getattr(battery, field) for field in SCOPE_FIELDS}
    if any(value < 0 for value in scope.values()) or scope["start_clock_time_ms"] == 0:
        raise InvalidReport("invalid clock")
    if not (
        scope["screen_off_uptime_ms"]
        <= scope["battery_uptime_ms"]
        <= scope["total_uptime_ms"]
        and scope["screen_off_realtime_ms"]
        <= scope["battery_realtime_ms"]
        <= scope["total_realtime_ms"]
        and scope["total_uptime_ms"] <= scope["total_realtime_ms"] + CLOCK_SKEW_MS
        and scope["screen_off_uptime_ms"] <= scope["screen_off_realtime_ms"] + CLOCK_SKEW_MS
        and scope["battery_uptime_ms"] <= scope["battery_realtime_ms"] + CLOCK_SKEW_MS
    ):
        raise InvalidReport("inconsistent clocks")
    matches = [entry for entry in stats.uids if entry.HasField("uid") and entry.uid == uid]
    if len(matches) > 1:
        raise InvalidReport("ambiguous UID")
    power = matches[0].power_use_item if matches else None
    charge = optional_charge(power, "computed_power_mah") if power is not None else None
    screen = optional_charge(power, "screen_power_mah") if power is not None else None
    return {
        "schema_version": 1,
        "kind": "android_uid_battery_report",
        "report_version": stats.report_version,
        "parcel_version": stats.parcel_version,
        "uid": uid,
        "uid_consumption_mah": charge,
        "uid_screen_consumption_mah": screen,
        "uid_attribution_available": charge is not None,
        "scope": scope,
        "raw_report_bytes": len(raw),
        "raw_report_sha256": hashlib.sha256(raw).hexdigest(),
        "single_endpoint_only": True,
        "physical_energy_measurement": False,
    }


def timed_out(_signum, _frame):
    raise InvalidReport("input timeout")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--uid", required=True, type=int)
    args = parser.parse_args()
    if not 0 <= args.uid < 2**31:
        parser.error("UID must be between 0 and 2147483647")
    old_handler = signal.signal(signal.SIGALRM, timed_out)
    signal.alarm(20)
    try:
        raw = sys.stdin.buffer.read(MAX_REPORT_BYTES + 1)
        result = decode(raw, args.uid)
        print(json.dumps(result, allow_nan=False, sort_keys=True))
    except (InvalidReport, DecodeError, OSError, subprocess.SubprocessError):
        print(
            "Cannot decode battery report: unsupported, unavailable or invalid input.",
            file=sys.stderr,
        )
        return 1
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old_handler)
    return 0


if __name__ == "__main__":
    sys.exit(main())

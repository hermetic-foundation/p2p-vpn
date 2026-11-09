"""Run a preregistered, endpoint-only Wi-Fi battery comparison on a test phone."""

import argparse
import base64
import hashlib
import importlib.util
import json
import math
import os
import re
import signal
import statistics
import subprocess
import sys
import time
import uuid
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "battery_decoder", Path(__file__).with_name("android-battery-decode.py")
)
decoder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decoder)
spec = importlib.util.spec_from_file_location(
    "paired_capture", Path(__file__).with_name("android-cellular-capture.py")
)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)
PACKAGE = "org.hermeticfoundation.p2pvpn.debug"
ORDER = ("reference", "candidate", "candidate", "reference", "reference", "candidate")
CHANGES = (
    "selection_changes",
    "selected_losses",
    "recoveries",
    "runtime_recovery_requests",
    "runtime_recovery_failures",
)
COUNTERS = (
    "packet_plane_sessions_expired",
    "packet_plane_path_demotions",
    "outbound_path_probe_failures",
    "packet_plane_path_recovery_dial_attempts",
    "packet_plane_path_recovery_dial_failures",
    "kad_primary_query_requests",
    "kad_primary_dial_intents_dispatched",
    "autonat_probes_scheduled",
    "kademlia_maintenance_cooldown_ticks",
)
PAYLOAD_COUNTERS = ("outbound_sent_packets", "inbound_accepted_packets")
MATCHED_CONTEXT = (
    "profile_sha256",
    "network_identity_sha256",
    "wifi_identity_sha256",
    "wifi_frequency_mhz",
    "power_save",
    "device_idle",
)


class InvalidTrial(ValueError):
    """A trial cannot support a matched comparison."""


def save(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, indent=2, allow_nan=False)
        stream.write("\n")


def file_hash(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate_plan(plan):
    if set(plan) != {
        "serial",
        "reference",
        "candidate",
        "expected_peers",
        "settle_seconds",
        "window_seconds",
    }:
        raise InvalidTrial("unexpected plan fields")
    if not isinstance(plan["serial"], str) or not re.fullmatch(
        r"[a-zA-Z0-9][a-zA-Z0-9-]{5,63}", plan["serial"]
    ):
        raise InvalidTrial("invalid serial")
    for key, low, high in (
        ("expected_peers", 1, 64),
        ("settle_seconds", 300, 1800),
        ("window_seconds", 1800, 7200),
    ):
        if type(plan[key]) is not int or not low <= plan[key] <= high:
            raise InvalidTrial("invalid trial bounds")
    for build in ("reference", "candidate"):
        item = plan[build]
        if (
            set(item) != {"apk", "sha256"}
            or not isinstance(item["apk"], str)
            or not Path(item["apk"]).is_absolute()
        ):
            raise InvalidTrial("invalid APK declaration")
        if not isinstance(item["sha256"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", item["sha256"]
        ):
            raise InvalidTrial("invalid APK hash")
    return plan


def summarize(rows):
    if len(rows) != len(ORDER) or [row["build"] for row in rows] != list(ORDER):
        return {
            "status": "inconclusive",
            "reason": "incomplete or reordered series",
            "goal_complete": False,
        }
    if any(not row.get("valid") for row in rows):
        return {
            "status": "inconclusive",
            "reason": "at least one invalid trial; none silently excluded",
            "goal_complete": False,
        }
    context = rows[0].get("comparison_context")
    if context is None or any(row.get("comparison_context") != context for row in rows):
        return {
            "status": "inconclusive",
            "reason": "unmatched cross-trial identity, radio or power policy",
            "goal_complete": False,
        }
    ratios = []
    for offset in (0, 2, 4):
        signals = [
            value
            for row in rows[offset : offset + 2]
            for value in row.get("wifi_rssi_dbm", [])
        ]
        if len(signals) != 4 or max(signals) - min(signals) > 5:
            return {
                "status": "inconclusive",
                "reason": "unmatched paired Wi-Fi signal",
                "goal_complete": False,
            }
        pair = {row["build"]: row["mah_per_hour"] for row in rows[offset : offset + 2]}
        if not all(math.isfinite(x) and x > 0 for x in pair.values()):
            return {
                "status": "inconclusive",
                "reason": "unresolvable energy rate",
                "goal_complete": False,
            }
        ratios.append(pair["candidate"] / pair["reference"])
    return {
        "status": "inconclusive"
        if min(ratios) <= 1 <= max(ratios)
        else "descriptive_paired_result",
        "candidate_over_reference_by_pair": ratios,
        "all_pairs_lower": all(ratio < 1 for ratio in ratios),
        "energy_target_observed_in_every_pair": all(ratio <= 0.1 for ratio in ratios),
        "reference_mean_mah_per_hour": statistics.mean(
            row["mah_per_hour"] for row in rows if row["build"] == "reference"
        ),
        "candidate_mean_mah_per_hour": statistics.mean(
            row["mah_per_hour"] for row in rows if row["build"] == "candidate"
        ),
        "uncertainty": "Three pairs are descriptive, not a precise confidence interval or whole-phone battery-life result",
        "goal_complete": False,
    }


def validate_window(start, end, seconds, peers):
    a, b = start["energy"], end["energy"]
    for key in ("uid", "report_version", "parcel_version"):
        if a[key] != b[key]:
            raise InvalidTrial("changed energy identity or schema")
    x, y = a["scope"], b["scope"]
    if any(x[key] != y[key] for key in ("start_clock_time_ms", "start_count")):
        raise InvalidTrial("battery statistics reset")
    elapsed = y["total_realtime_ms"] - x["total_realtime_ms"]
    battery = y["battery_realtime_ms"] - x["battery_realtime_ms"]
    off = y["screen_off_realtime_ms"] - x["screen_off_realtime_ms"]
    if (
        not seconds * 1000 <= elapsed <= (seconds + 5) * 1000
        or abs(battery - elapsed) > 1000
        or battery != off
    ):
        raise InvalidTrial("duration, charging accounting or screen interruption")
    values = [a["uid_consumption_mah"], b["uid_consumption_mah"]]
    if (
        not all(
            type(value) in (int, float) and math.isfinite(value) and value >= 0
            for value in values
        )
        or values[1] < values[0]
    ):
        raise InvalidTrial("unavailable or nonmonotonic energy")
    p, q = start["context"], end["context"]
    for key in ("apk_sha256", "generation", "application_pid", *MATCHED_CONTEXT):
        if p[key] != q[key]:
            raise InvalidTrial("changed build, identity, session or Wi-Fi")
    for context in (p, q):
        if (
            not context["connected"]
            or context["busy"]
            or context["pairing"]
            or context["underlay"]["kind"] != "wifi"
            or not context["underlay"]["validated"]
        ):
            raise InvalidTrial("not a settled Wi-Fi VPN")
        if (
            context["paths"]["connected_peers"] != peers
            or context["paths"]["packet_plane_quic_sessions"] != peers
        ):
            raise InvalidTrial("endpoint peer or QUIC topology mismatch")
        if context["thermal_status"] != 0 or context["wifi_rssi_dbm"] < -65:
            raise InvalidTrial("thermal throttling or weak Wi-Fi")
        if type(context["power_save"]) is not bool or not re.fullmatch(
            r"[A-Z_]{1,32}", context["device_idle"]
        ):
            raise InvalidTrial("unavailable power policy")
    if (
        any(p["underlay"][key] != q["underlay"][key] for key in CHANGES)
        or abs(p["wifi_rssi_dbm"] - q["wifi_rssi_dbm"]) > 5
    ):
        raise InvalidTrial("network transition or signal drift")
    traffic = (
        "outbound_quic_datagram_packets",
        "outbound_owned_quic_datagram_packets",
        "outbound_owned_udp_datagram_packets",
        "outbound_direct_tcp_stream_packets",
    )
    if any(p["paths"][key] != q["paths"][key] for key in traffic):
        raise InvalidTrial("application traffic during connected-idle workload")
    deltas = []
    if len(p["counters"]) != len(q["counters"]):
        raise InvalidTrial("changed runtime network count")
    for old, new in zip(p["counters"], q["counters"], strict=True):
        old, new = (
            dict(line.split(" ", 1) for line in values) for values in (old, new)
        )
        changes = {
            key: int(new[key]) - int(old[key]) if key in old and key in new else None
            for key in (*COUNTERS, *PAYLOAD_COUNTERS)
        }
        if any(value is not None and value < 0 for value in changes.values()):
            raise InvalidTrial("cumulative runtime counter reset")
        if any(changes[key] != 0 for key in PAYLOAD_COUNTERS):
            raise InvalidTrial(
                "missing payload counters or non-idle application traffic"
            )
        deltas.append(changes)
    return {
        "valid": True,
        "elapsed_ms": elapsed,
        "delta_mah": values[1] - values[0],
        "mah_per_hour": (values[1] - values[0]) * 3600000 / elapsed,
        "secondary_counter_deltas": deltas,
        "wifi_rssi_dbm": [p["wifi_rssi_dbm"], q["wifi_rssi_dbm"]],
        "comparison_context": {
            **{key: p[key] for key in MATCHED_CONTEXT},
            **{key: a[key] for key in ("uid", "report_version", "parcel_version")},
        },
    }


class Phone:
    def __init__(self, serial):
        self.serial = serial
        self.device = capture.Device(serial)
        self.fallback = None

    def adb(self, *args):
        try:
            return self.device.adb(*args, limit=decoder.MAX_REPORT_BYTES, timeout=60)[1]
        except capture.compare.InvalidWindow as error:
            raise InvalidTrial("bounded ADB request failed") from error

    def shell(self, *args):
        try:
            return self.device.shell(*args)
        except capture.compare.InvalidWindow as error:
            raise InvalidTrial("bounded shell request failed") from error

    def preflight(self):
        self.device.usb()
        battery = self.shell("dumpsys", "battery")
        if "UPDATES STOPPED" in battery or "USB powered: true" not in battery:
            raise InvalidTrial("requires USB power and normal battery reporting")
        if self.shell(
            "find",
            "/data/local/tmp",
            "-maxdepth",
            "1",
            "-name",
            "p2p-vpn-battery-*.armed",
        ):
            raise InvalidTrial("another battery override is pending")

    def begin_accounting(self, seconds):
        if self.fallback is not None:
            raise InvalidTrial("previous battery restoration is incomplete")
        tag = "p2p-vpn-battery-wifi-" + uuid.uuid4().hex
        pid = self.device.fallback(tag, seconds + 300)
        self.fallback = (pid, tag)
        self.device.begin_override(pid, tag)

    def rpc(self, command):
        text = self.shell(
            "am",
            "broadcast",
            "--receiver-foreground",
            "-a",
            PACKAGE + ".AUTOMATION",
            "-n",
            PACKAGE + "/org.hermeticfoundation.p2pvpn.DebugAutomationReceiver",
            "--es",
            "command",
            command,
        )
        match = re.search(r'data="([A-Za-z0-9+/=]+)"', text)
        if not match:
            raise InvalidTrial("missing automation response")
        result = json.loads(base64.b64decode(match.group(1), validate=True))
        if not result.get("ok"):
            raise InvalidTrial("automation request failed")
        return result["value"]

    def open(self):
        self.shell("input", "keyevent", "KEYCODE_WAKEUP")
        self.shell("wm", "dismiss-keyguard")
        self.shell(
            "am",
            "start",
            "-W",
            "-n",
            PACKAGE + "/org.hermeticfoundation.p2pvpn.MainActivity",
        )

    def context(self):
        status = self.rpc("status")["snapshot"]
        resources = self.rpc("resource-status")
        if (
            sum(network["enabled"] for network in status["networks"]) != 1
            or len(resources["networks"]) != 1
            or resources["networks"][0]["phase"] != "running"
        ):
            raise InvalidTrial("comparison requires one running network")
        wifi = self.shell("cmd", "wifi", "status")
        radio = re.search(
            r"BSSID: ([0-9a-fA-F:]{17}),.*?RSSI: (-?\d+),.*?Frequency: (\d+)MHz", wifi
        )
        thermal = self.shell("dumpsys", "thermalservice")
        throttle = re.search(r"^Thermal Status: (\d+)$", thermal, re.MULTILINE)
        power_save = self.shell("settings", "get", "global", "low_power")
        idle = re.search(r"\bmState=(\w+)", self.shell("dumpsys", "deviceidle"))
        if not radio or not throttle or power_save not in ("0", "1") or not idle:
            raise InvalidTrial("missing radio or thermal admission data")
        apk_path = self.shell("pm", "path", PACKAGE)
        if not re.fullmatch(r"package:/data/app/[^\n]+\.apk", apk_path):
            raise InvalidTrial("unexpected installed APK layout")
        identity = [
            {key: item[key] for key in ("id", "peer_id", "enabled")}
            for item in status["networks"]
        ]
        pid = self.shell("pidof", PACKAGE)
        if not re.fullmatch(r"[1-9]\d*", pid):
            raise InvalidTrial("unexpected app process layout")
        return {
            "apk_sha256": self.shell(
                "sha256sum", apk_path.removeprefix("package:")
            ).split()[0],
            "profile_sha256": self.shell(
                "run-as", PACKAGE, "sha256sum", "no_backup/profile.enc"
            ).split()[0],
            "network_identity_sha256": hashlib.sha256(
                json.dumps(
                    sorted(identity, key=lambda row: row["id"]), sort_keys=True
                ).encode()
            ).hexdigest(),
            "wifi_identity_sha256": hashlib.sha256(
                radio.group(1).lower().encode()
            ).hexdigest(),
            "wifi_rssi_dbm": int(radio.group(2)),
            "wifi_frequency_mhz": int(radio.group(3)),
            "thermal_status": int(throttle.group(1)),
            "power_save": power_save == "1",
            "device_idle": idle.group(1),
            "application_pid": int(pid),
            "generation": status["runtime_generation"],
            "connected": status["connected"],
            "busy": status["busy"],
            "pairing": any(
                status["pairing"][key] is not None for key in ("code", "candidate_peer")
            ),
            "underlay": status["underlay"],
            "paths": status["paths"],
            "counters": [network["counters"] for network in resources["networks"]],
        }

    def energy(self, uid, root, name):
        raw = self.adb("exec-out", "dumpsys", "batterystats", "--proto", PACKAGE)
        capture.save(root, name, raw)
        return decoder.decode(raw, uid)

    def restore(self):
        failures = []
        if self.fallback is not None:
            try:
                self.device.restore(*self.fallback)
            except capture.compare.MALFORMED_EVIDENCE:
                failures.append("battery_reporting")
            else:
                self.fallback = None
        try:
            self.shell("input", "keyevent", "KEYCODE_SLEEP")
        except capture.compare.MALFORMED_EVIDENCE:
            failures.append("screen")
        return failures


def trial(phone, plan, build, root, profile_hash):
    item = plan[build]
    if (
        not 0 < Path(item["apk"]).stat().st_size <= 64 * 1024 * 1024
        or file_hash(item["apk"]) != item["sha256"]
    ):
        raise InvalidTrial("APK file changed")
    phone.adb("install", "-r", item["apk"])
    phone.open()
    status = phone.rpc("status")["snapshot"]
    if status["always_on"] or status["lockdown"] or status["connected"]:
        raise InvalidTrial("test requires an initially disconnected, non-always-on VPN")
    phone.rpc("connect")
    phone.shell("input", "keyevent", "KEYCODE_SLEEP")
    deadline = time.monotonic() + 180
    while True:
        status = phone.rpc("status")["snapshot"]
        if (
            status["paths"]["connected_peers"] == plan["expected_peers"]
            and status["paths"]["packet_plane_quic_sessions"] == plan["expected_peers"]
        ):
            break
        if time.monotonic() >= deadline:
            raise InvalidTrial("readiness deadline exceeded")
        time.sleep(5)
    phone.begin_accounting(plan["settle_seconds"] + plan["window_seconds"])
    print(
        json.dumps(
            {"stage": "settling", "build": build, "seconds": plan["settle_seconds"]}
        ),
        flush=True,
    )
    time.sleep(plan["settle_seconds"])
    context = phone.context()
    uid_text = phone.shell("cmd", "package", "list", "packages", "-U", PACKAGE)
    uid_match = re.fullmatch(r"package:" + re.escape(PACKAGE) + r" uid:(\d+)", uid_text)
    if (
        not uid_match
        or context["apk_sha256"] != item["sha256"]
        or context["profile_sha256"] != profile_hash
    ):
        raise InvalidTrial("build, profile or UID admission mismatch")
    uid = int(uid_match.group(1))
    start = {
        "context": context,
        "energy": phone.energy(uid, root, "start.pb"),
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    save(root / "start.json", start)
    print(
        json.dumps(
            {"stage": "measuring", "build": build, "seconds": plan["window_seconds"]}
        ),
        flush=True,
    )
    time.sleep(plan["window_seconds"])
    energy = phone.energy(uid, root, "end.pb")
    end = {
        "context": phone.context(),
        "energy": energy,
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    save(root / "end.json", end)
    return validate_window(start, end, plan["window_seconds"], plan["expected_peers"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    if args.plan.stat().st_size > 16384:
        raise InvalidTrial("plan exceeds byte limit")
    plan = validate_plan(json.loads(args.plan.read_text()))
    with capture.device_lock(plan["serial"]):
        return run(plan, args.output)


def run(plan, output):
    os.umask(0o077)
    output.mkdir(mode=0o700, parents=False, exist_ok=False)
    save(
        output / "plan.json",
        {
            **plan,
            "order": ORDER,
            "package": PACKAGE,
            "interpretation": "Legacy connected-idle Wi-Fi attribution; not a modem-energy or demand-lifecycle benchmark",
        },
    )
    phone = Phone(plan["serial"])
    phone.preflight()
    initial = phone.rpc("status")["snapshot"]
    if (
        initial["connected"]
        or initial["always_on"]
        or initial["lockdown"]
        or any(
            initial["pairing"][key] is not None for key in ("code", "candidate_peer")
        )
    ):
        raise InvalidTrial(
            "test requires a disconnected test profile without always-on or active pairing"
        )
    profile = phone.shell(
        "run-as", PACKAGE, "sha256sum", "no_backup/profile.enc"
    ).split()[0]
    save(
        output / "device.json",
        {
            "model": phone.shell("getprop", "ro.product.model"),
            "sdk": phone.shell("getprop", "ro.build.version.sdk"),
            "fingerprint": phone.shell("getprop", "ro.build.fingerprint"),
            "profile_sha256": profile,
        },
    )
    rows = []
    interrupted = False
    try:
        for index, build in enumerate(ORDER):
            root = output / f"trial-{index + 1}-{build}"
            root.mkdir()
            row = {"build": build, "valid": False}
            try:
                signal.alarm(plan["settle_seconds"] + plan["window_seconds"] + 600)
                row.update(trial(phone, plan, build, root, profile))
            except InvalidTrial as error:
                row["failure"] = str(error)
            except (*capture.compare.MALFORMED_EVIDENCE, KeyboardInterrupt):
                row["failure"] = "interrupted or failed device operation"
                interrupted = True
            finally:
                signal.alarm(0)
                try:
                    phone.rpc("disconnect")
                except capture.compare.MALFORMED_EVIDENCE:
                    row["disconnect_failed"] = True
                    interrupted = True
                row["restoration_failures"] = phone.restore()
                if row["restoration_failures"]:
                    interrupted = True
                if interrupted:
                    row["valid"] = False
                save(root / "result.json", row)
                rows.append(row)
                print(json.dumps({"trial": index + 1, **row}), flush=True)
            if interrupted:
                break
    finally:
        save(output / "summary.json", summarize(rows))
        if phone.restore():
            interrupted = True
    return 1 if interrupted or not all(row.get("valid") for row in rows) else 0


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGALRM, interrupted)
    try:
        sys.exit(main())
    except (
        InvalidTrial,
        OSError,
        ValueError,
        KeyError,
        TypeError,
        subprocess.SubprocessError,
    ):
        print(
            "Battery comparison failed; inspect the private trial records.",
            file=sys.stderr,
        )
        sys.exit(1)

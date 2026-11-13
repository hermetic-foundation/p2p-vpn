"""Capture one planned screen-off cellular window on an explicit USB device."""

import argparse
import base64
import contextlib
import datetime
import fcntl
import hashlib
import importlib.util
import ipaddress
import json
import os
import re
import resource
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from decimal import Decimal
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "cellular_compare", Path(__file__).with_name("android-cellular-compare.py")
)
compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare)
require = compare.require
PACKAGE = "org.hermeticfoundation.p2pvpn.debug"
MAX_COMMAND = 256 * 1024
MAX_TRACE = 32 * 1024 * 1024
# Observe the round trip, not just the runtime's 30-second queue retention.
FIRST_WAKE_REPLY_WAIT_SECONDS = 45
WAKE_PROBE_TIMEOUT_SECONDS = FIRST_WAKE_REPLY_WAIT_SECONDS + 5
STEADY_PROBE_TIMEOUT_SECONDS = 15
BOUNDARY_PROBE_TIMEOUT_SECONDS = (
    WAKE_PROBE_TIMEOUT_SECONDS + STEADY_PROBE_TIMEOUT_SECONDS
)
FALLBACK_ALLOWANCE_SECONDS = 360 + 2 * BOUNDARY_PROBE_TIMEOUT_SECONDS
WATCHDOG_ALLOWANCE_SECONDS = 480 + 4 * BOUNDARY_PROBE_TIMEOUT_SECONDS


class InterruptedCapture(Exception):
    pass


class CommandFailure(compare.InvalidWindow):
    def __init__(self, code, stdout, stderr, returncode):
        super().__init__(code)
        self.stdout = stdout
        self.stderr = stderr
        self.returncode = returncode


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def save(directory, name, value):
    raw = (
        value
        if isinstance(value, bytes)
        else json.dumps(value, sort_keys=True, allow_nan=False).encode()
    )
    fd = os.open(
        directory / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
    )
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)


def emit(stage, **values):
    print(json.dumps({"stage": stage, **values}, sort_keys=True), flush=True)


def bounded_run(args, limit=MAX_COMMAND, timeout=30, check=True):
    def bounds():
        resource.setrlimit(resource.RLIMIT_FSIZE, (limit, limit))

    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        code, failure = None, None
        try:
            code = subprocess.run(
                args,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=errors,
                timeout=timeout,
                preexec_fn=bounds,
                check=False,
            ).returncode
        except subprocess.TimeoutExpired:
            failure = "command_timeout"
        except InterruptedCapture:
            failure = "command_interrupted"
        output.seek(0)
        raw = output.read(limit + 1)
        errors.seek(0)
        error_raw = errors.read(limit + 1)
    if failure or (check and code != 0):
        raise CommandFailure(
            failure or "command_failed", raw[:limit], error_raw[:limit], code
        )
    require(len(raw) <= limit, "command_output_bounds")
    return code, raw


@contextlib.contextmanager
def device_lock(serial):
    root = Path.home() / ".cache/p2p-vpn/battery-capture-locks"
    root.mkdir(parents=True, mode=0o700, exist_ok=True)
    require(
        not root.is_symlink()
        and root.stat().st_uid == os.getuid()
        and root.stat().st_mode & 0o077 == 0,
        "lock_directory",
    )
    fd = os.open(
        root / (sha(serial.encode()) + ".lock"),
        os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW,
        0o600,
    )
    with os.fdopen(fd, "rb") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise compare.InvalidWindow("device_busy") from error
        yield


def validate_workload(value):
    require(
        set(value)
        == {
            "schema_version",
            "network_name",
            "phone_peer_id",
            "phone_address",
            "peer_address",
            "authorized_peer_ids",
            "counterpart_builds",
            "diagnostic_overrides",
        },
        "workload",
    )
    compare.integer(value["schema_version"], 1, 1, "workload")
    require(
        isinstance(value["network_name"], str)
        and re.fullmatch(r"[a-zA-Z0-9_-]{1,63}", value["network_name"]),
        "workload_network",
    )
    peers = value["authorized_peer_ids"]
    require(isinstance(peers, list) and 1 <= len(peers) <= 128, "workload_peers")
    require(
        all(
            isinstance(p, str) and re.fullmatch(r"[a-zA-Z0-9]{16,128}", p)
            for p in [value["phone_peer_id"], *peers]
        ),
        "workload_peers",
    )
    require(
        len(set(peers)) == len(peers) and value["phone_peer_id"] not in peers,
        "workload_peers",
    )
    for key in ("phone_address", "peer_address"):
        address = ipaddress.ip_address(value[key])
        require(
            address.version == 4
            and not address.is_unspecified
            and not address.is_multicast
            and not address.is_loopback
            and str(address) == value[key],
            "workload_address",
        )
    require(value["phone_address"] != value["peer_address"], "workload_address")
    builds = value["counterpart_builds"]
    require(isinstance(builds, dict) and 1 <= len(builds) <= 128, "counterpart_builds")
    for name, digest in builds.items():
        require(re.fullmatch(r"[a-zA-Z0-9_-]{1,63}", name), "counterpart_builds")
        compare.digest(digest)
    require(value["diagnostic_overrides"] is False, "diagnostic_override")


def trace_config(seconds):
    return f"""buffers {{ size_kb: 8192 fill_policy: RING_BUFFER }}
duration_ms: {(seconds + 180) * 1000}
flush_period_ms: 5000
write_into_file: true
file_write_period_ms: 5000
max_file_size_bytes: {MAX_TRACE}
data_sources {{ config {{ name: "android.power" android_power_config {{
  battery_poll_ms: 1000 collect_power_rails: true
}} }} }}
""".encode()


def initialize(directory, reference, candidate, workload_file, seconds, settle):
    compare.integer(seconds, 1800, 7200, "window_duration")
    compare.integer(settle, 300, 1800, "settling")
    raw = compare.read_file(workload_file, compare.MAX_JSON)
    validate_workload(compare.parse_json(raw))
    builds = [
        sha(compare.read_file(path, 64 * 1024 * 1024))
        for path in (reference, candidate)
    ]
    require(builds[0] != builds[1], "same_apk")
    directory.mkdir(mode=0o700)
    save(directory, "workload.json", raw)
    save(directory, "trace.pbtxt", trace_config(seconds))
    save(
        directory,
        "plan.json",
        {
            "schema_version": 1,
            "reference_apk": builds[0],
            "candidate_apk": builds[1],
            "window_seconds": seconds,
            "settle_seconds": settle,
            "windows": ["01-A", "02-B", "03-B", "04-A", "05-A", "06-B"],
            "workload_sha256": sha(raw),
            "trace_config_sha256": sha(trace_config(seconds)),
            "created_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "collector_source_sha256": sha(Path(__file__).read_bytes()),
        },
    )


def subscription_id(network_line):
    values = []
    for group in re.findall(r"SubscriptionIds:\s*[\[{]([^\]}]*)[\]}]", network_line):
        values.extend(value.strip() for value in group.split(",") if value.strip())
    values += re.findall(r"\bmSubId\s*=\s*([0-9]+)", network_line)
    require(
        values and all(value.isdecimal() for value in values) and len(set(values)) == 1,
        "serving_subscription_unavailable",
    )
    return int(values[0])


def parse_signal(raw, selected_sub_id):
    require(
        "last known state:" in raw and "local logs:" in raw, "radio_state_unavailable"
    )
    current = raw.split("last known state:", 1)[1].split("local logs:", 1)[0]

    def field(name):
        values = re.findall(r"(?m)^[ \t]*" + name + r"=([0-9]+)[ \t]*$", current)
        require(len(values) == 1, "serving_subscription_unavailable")
        return int(values[0])

    require(
        field("mActiveDataSubId") == selected_sub_id
        and field("mDefaultSubId") == selected_sub_id,
        "serving_subscription_mapping_unavailable",
    )
    phone_id = field("mDefaultPhoneId")
    blocks = re.findall(
        r"(?ms)^[ \t]*Phone Id=([0-9]+)[ \t]*$(.*?)(?=^[ \t]*(?:Phone Id=|mPhoneCapability=)|\Z)",
        current,
    )
    matches = [body for phone, body in blocks if int(phone) == phone_id]
    require(len(matches) == 1, "serving_phone_unavailable")
    lines = [line for line in matches[0].splitlines() if "mSignalStrength=" in line]
    values = sorted(
        {
            int(value)
            for line in lines
            for value in re.findall(r"\b(?:rsrp|ssRsrp)=(-?\d+)", line)
            if -120 <= int(value) <= -40
        }
    )
    require(len(values) == 1, "ambiguous_serving_signal")
    return {
        "subscription_id": selected_sub_id,
        "phone_id": phone_id,
        "rsrp_dbm": values[0],
    }


def live_temperature(raw):
    require("Current temperatures from HAL:" in raw, "thermal_unavailable")
    current = raw.split("Current temperatures from HAL:", 1)[1].split(
        "Current cooling devices", 1
    )[0]
    values = [
        float(value)
        for value in re.findall(
            r"Temperature\{mValue=([-+\d.]+), mType=2, mName=battery,", current
        )
    ]
    require(values and all(18 <= value <= 40 for value in values), "temperature")
    status = re.search(r"^Thermal Status: (\d+)$", raw, re.MULTILINE)
    require(status and status.group(1) == "0", "thermal")
    return round(max(values) * 10)


def validated_cellular(raw):
    active = re.search(r"Active default network: (\d+)", raw)
    line = next(
        (
            line
            for line in raw.splitlines()
            if active and "NetworkAgentInfo{network{" + active.group(1) + "}" in line
        ),
        "",
    )
    require(
        bool(re.search(r"ni\{MOBILE(?:\[|\s)", line)) and "IS_VALIDATED" in line,
        "cellular_underlay",
    )
    return line


def screen_off(raw):
    require(
        bool(re.search(r"\bmWakefulness=(?:Asleep|Dozing)\b", raw))
        and "mHalInteractiveModeEnabled=false" in raw,
        "screen_wake",
    )


class Device:
    def __init__(self, serial):
        require(
            isinstance(serial, str)
            and re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9-]{5,63}", serial),
            "usb_serial",
        )
        self.serial = serial

    def adb(self, *args, **kwargs):
        return bounded_run(["adb", "-s", self.serial, *args], **kwargs)

    def shell(self, *args, check=True):
        return self.adb("shell", shlex.join(args), check=check)[1].decode().strip()

    def rpc(self, command):
        raw = self.shell(
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
        match = re.search(r'data="([a-zA-Z0-9+/=]+)"', raw)
        require(match, "automation_response")
        value = compare.parse_json(base64.b64decode(match.group(1), validate=True))
        require(
            value["schema_version"] == 1 and value["ok"] is True,
            "automation_unavailable",
        )
        return value["value"]

    def file_hash(self, *args):
        value = self.shell(*args).split()
        require(value, "device_hash")
        return compare.digest(value[0])

    def usb(self):
        require(
            self.adb("get-devpath")[1].decode().strip().startswith("usb:"),
            "usb_transport",
        )

    def radio_state(self):
        # Discard historical entries on-device; their size grows with phone uptime.
        return self.shell(
            "sh",
            "-c",
            "set -o pipefail; dumpsys telephony.registry "
            "| sed -n '/last known state:/,/local logs:/p'",
        )

    def network_state(self):
        return self.shell(
            "sh",
            "-c",
            "set -o pipefail; dumpsys connectivity | grep -E "
            "'^[[:space:]]*(Active default network:|NetworkAgentInfo[{]network[{])'",
        )

    def preflight(self, workload):
        self.usb()
        screen_off(self.shell("dumpsys", "power"))
        network_line = validated_cellular(self.network_state())
        parse_signal(self.radio_state(), subscription_id(network_line))
        live_temperature(self.shell("dumpsys", "thermalservice"))
        require(
            self.shell("settings", "get", "global", "wifi_on") == "0", "wifi_enabled"
        )
        raw = self.shell("dumpsys", "battery")
        require(
            "UPDATES STOPPED" not in raw and "USB powered: true" in raw,
            "existing_battery_override_or_power",
        )
        require(
            not self.shell(
                "find",
                "/data/local/tmp",
                "-maxdepth",
                "1",
                "-name",
                "p2p-vpn-battery-*.armed",
            ),
            "pending_battery_override",
        )
        # Prove the host's inbound probe uses a TUN, not a coincidentally matching LAN route.
        route = json.loads(
            bounded_run(["ip", "-j", "route", "get", workload["phone_address"]])[1]
        )
        require(len(route) == 1 and isinstance(route[0]["dev"], str), "overlay_route")
        link = json.loads(
            bounded_run(["ip", "-d", "-j", "link", "show", "dev", route[0]["dev"]])[1]
        )
        require(
            len(link) == 1 and link[0]["linkinfo"]["info_kind"] == "tun",
            "overlay_route",
        )

    def identities(self, workload):
        self.usb()
        # resource-status does not start an absent service; check it before status.
        resources = self.rpc("resource-status")
        status = self.rpc("status")
        require(status["service_ready"] is True, "service_not_ready")
        snapshot = status["snapshot"]
        require(
            snapshot["connected"] is True and snapshot["busy"] is False,
            "vpn_not_running",
        )
        require(
            not any(snapshot["pairing"].get(key) for key in ("code", "candidate_peer")),
            "pairing_active",
        )
        networks = [n for n in snapshot["networks"] if n["enabled"]]
        require(len(networks) == 1, "enabled_networks")
        network = networks[0]
        require(
            isinstance(network["addresses"], list)
            and 1 <= len(network["addresses"]) <= 16
            and all(
                isinstance(address, str) and len(address) <= 64
                for address in network["addresses"]
            ),
            "network_identity",
        )
        require(
            network["name"] == workload["network_name"]
            and network["peer_id"] == workload["phone_peer_id"]
            and ipaddress.ip_address(workload["phone_address"])
            in [ipaddress.ip_interface(address).ip for address in network["addresses"]],
            "network_identity",
        )
        require(isinstance(network["id"], str), "network_identity")
        network_id = str(uuid.UUID(network["id"]))
        require(network_id == network["id"], "network_identity")
        entries = [n for n in resources["networks"] if n["id"] == network_id]
        require(
            len(entries) == 1 and entries[0]["phase"] == "running", "native_not_running"
        )
        counters = {}
        for line in entries[0]["counters"]:
            match = re.fullmatch(r"([a-z0-9_]+) ([0-9]{1,20})", line)
            require(match and match.group(1) not in counters, "native_counters")
            counters[match.group(1)] = int(match.group(2))
        for key in ("path_probe_diagnostic_paused", "kad_primary_diagnostic_paused"):
            require(counters.get(key, 0) == 0, "diagnostic_override")
        paths = self.shell("pm", "path", "--user", "0", PACKAGE).splitlines()
        require(
            len(paths) == 1 and paths[0].startswith("package:/data/app/"),
            "installed_apk",
        )
        apk = self.file_hash("sha256sum", paths[0].removeprefix("package:"))
        profile = self.file_hash(
            "run-as", PACKAGE, "sha256sum", "no_backup/profile.enc"
        )
        membership = self.file_hash(
            "run-as",
            PACKAGE,
            "sha256sum",
            f"no_backup/runtime/{network_id}/membership-state.json",
        )
        identity = {
            "device": sha(self.serial.encode()),
            "os_build": sha(self.shell("getprop", "ro.build.fingerprint").encode()),
            "apk": apk,
            "profile": sha(
                json.dumps(
                    {"profile": profile, "membership": membership}, sort_keys=True
                ).encode()
            ),
            "peer_inventory": sha(
                json.dumps(sorted(workload["authorized_peer_ids"])).encode()
            ),
        }
        # Retain selected public/runtime evidence, never pairing codes or telephony registry.
        evidence = {
            "network": network,
            "underlay": snapshot["underlay"],
            "paths": snapshot["paths"],
            "runtime_generation": snapshot["runtime_generation"],
            "counters": counters,
            "profile_sha256": profile,
            "membership_sha256": membership,
        }
        return identity, evidence

    def context(self, workload, evidence):
        network_line = validated_cellular(self.network_state())
        radio = parse_signal(self.radio_state(), subscription_id(network_line))
        evidence["radio"] = radio
        screen_off(self.shell("dumpsys", "power"))
        wifi = self.shell("settings", "get", "global", "wifi_on")
        require(wifi == "0", "wifi_enabled")
        low_power = self.shell("settings", "get", "global", "low_power")
        require(low_power in ("0", "1"), "power_policy")
        idle = re.search(r"\bmState=(\w+)", self.shell("dumpsys", "deviceidle"))
        require(idle, "device_idle")
        pid = self.shell("pidof", PACKAGE)
        require(pid.isdecimal(), "app_process")
        battery = self.shell("dumpsys", "battery")
        require(
            "UPDATES STOPPED" in battery and "USB powered: false" in battery,
            "battery_accounting",
        )
        underlay = evidence["underlay"]
        require(
            underlay["kind"] == "cellular" and underlay["validated"] is True,
            "cellular_underlay",
        )
        return {
            "app_pid": int(pid),
            "runtime_generation": evidence["runtime_generation"],
            "screen_interactive": False,
            "wifi_enabled": False,
            "underlay_kind": "cellular",
            "underlay_validated": True,
            "underlay_generation": sha(
                json.dumps(
                    {
                        "runtime": underlay,
                        "radio_binding": {
                            key: value
                            for key, value in radio.items()
                            if key != "rsrp_dbm"
                        },
                    },
                    sort_keys=True,
                ).encode()
            ),
            "accounting_mode": "simulated_on_battery",
            "physical_usb_powered": True,
            "power_save": low_power == "1",
            "device_idle": idle.group(1),
            "thermal_status": 0,
            "temperature_tenths_c": live_temperature(
                self.shell("dumpsys", "thermalservice")
            ),
            "rsrp_dbm": radio["rsrp_dbm"],
            "authorized_peers": len(workload["authorized_peer_ids"]),
            "payload_sent_packets": evidence["counters"]["outbound_sent_packets"],
            "payload_received_packets": evidence["counters"][
                "inbound_accepted_packets"
            ],
        }

    def anchor(self):
        lines = self.shell(
            "cat", "/proc/sys/kernel/random/boot_id", "/proc/uptime"
        ).splitlines()
        require(len(lines) == 2, "clock_bracket")
        boot = str(uuid.UUID(lines[0]))
        value = lines[1].split()[0]
        require(re.fullmatch(r"[0-9]{1,12}\.[0-9]{1,3}", value), "clock_bracket")
        uptime = int(Decimal(value) * 1000)
        return boot, uptime

    def endpoint(self, directory, name, context, uid):
        before = self.anchor()
        failure = None
        try:
            raw = self.adb(
                "exec-out",
                "dumpsys",
                "batterystats",
                "--proto",
                PACKAGE,
                limit=compare.decoder.MAX_REPORT_BYTES,
            )[1]
        except CommandFailure as error:
            raw, failure = error.stdout, error
            save(directory, name + "-command-stderr.txt", error.stderr)
            save(
                directory,
                name + "-command-error.json",
                {"error": str(error), "returncode": error.returncode},
            )
        save(directory, name + ".pb", raw)
        if failure and str(failure) == "command_interrupted":
            save(directory, name + "-clock.json", {"before": before, "after": None})
            raise InterruptedCapture()
        after = self.anchor()
        save(directory, name + "-clock.json", {"before": before, "after": after})
        if failure:
            raise failure
        compare.decoder.decode(raw, uid)
        require(before[0] == after[0], "boot_changed")
        return {
            "boot_id": before[0],
            "before_uptime_ms": before[1],
            "after_uptime_ms": after[1],
            "context": context,
        }

    def uid(self):
        raw = self.shell(
            "cmd", "package", "list", "packages", "--user", "0", "-U", PACKAGE
        )
        match = re.fullmatch("package:" + re.escape(PACKAGE) + r" uid:(\d+)", raw)
        require(match, "package_uid")
        return int(match.group(1))

    def fallback(self, tag, seconds):
        original = self.shell("dumpsys", "battery")
        require(
            "UPDATES STOPPED" not in original and "USB powered: true" in original,
            "existing_battery_override_or_power",
        )
        ready = "/data/local/tmp/" + tag + ".ready"
        armed = "/data/local/tmp/" + tag + ".armed"
        script = f"""umask 077; sleep {seconds} & child=$!; trap 'kill "$child" 2>/dev/null; rm -f -- {ready} {armed}; exit' TERM INT; kill -0 "$child" || exit 1; printf '%s:%s\\n' "$$" "$child" > {ready} || exit 1; wait "$child"; if test -f {armed}; then dumpsys battery reset -f; fi; rm -f -- {ready} {armed}"""
        command = (
            "nohup "
            + shlex.join(["sh", "-c", script, tag])
            + " >/dev/null 2>&1 </dev/null & echo $!"
        )
        pid = self.shell("sh", "-c", command)
        require(pid.isdecimal() and int(pid) > 1, "fallback_pid")
        pid = int(pid)
        try:
            for _ in range(10):
                acknowledgement = self.shell("cat", ready, check=False)
                match = re.fullmatch(str(pid) + r":([0-9]+)", acknowledgement)
                if match:
                    parent = self.process_args(pid)
                    child = self.process_args(int(match.group(1)))
                    require(
                        parent
                        and tag in parent
                        and child
                        and (
                            (
                                Path(child[0]).name == "sleep"
                                and child[1:] == [str(seconds)]
                            )
                            or (
                                Path(child[0]).name == "toybox"
                                and child[1:] == ["sleep", str(seconds)]
                            )
                        ),
                        "fallback_not_ready",
                    )
                    return pid
                time.sleep(0.1)
            raise compare.InvalidWindow("fallback_not_ready")
        except (compare.InvalidWindow, *compare.MALFORMED_EVIDENCE):
            with contextlib.suppress(
                compare.InvalidWindow, *compare.MALFORMED_EVIDENCE
            ):
                self.stop_owned(pid, tag, "-TERM")
            raise

    def begin_override(self, pid, tag):
        args = self.process_args(pid)
        require(args and tag in args, "fallback_not_ready")
        self.shell(
            "sh",
            "-c",
            "umask 077; touch " + shlex.quote("/data/local/tmp/" + tag + ".armed"),
        )
        self.shell("dumpsys", "battery", "unplug", "-f")

    def process_args(self, pid):
        code, raw = self.adb(
            "shell", shlex.join(["cat", f"/proc/{pid}/cmdline"]), check=False
        )
        if code != 0:
            probe = (
                f"if test -d /proc/{pid}; then printf present; else printf absent; fi"
            )
            exists, response = self.adb(
                "shell", shlex.join(["sh", "-c", probe]), check=False
            )
            require(
                exists == 0 and response == b"absent", "process_inspection_unavailable"
            )
            return None
        if not raw:
            status = self.shell("cat", f"/proc/{pid}/status")
            require(
                bool(re.search(r"^State:\s+Z\b", status, re.MULTILINE)),
                "process_inspection_unavailable",
            )
            return None
        return (
            raw.decode().split("\x00")[0:-1]
            if raw.endswith(b"\x00")
            else raw.decode().split("\x00")
        )

    def stop_owned(self, pid, tag, sig):
        args = self.process_args(pid)
        if args is None:
            return
        require(tag in args, "process_ownership")
        self.shell("kill", sig, str(pid))
        for _ in range(20):
            args = self.process_args(pid)
            if args is None or tag not in args:
                return
            time.sleep(0.25)
        raise compare.InvalidWindow("process_stop_unverified")

    def restore(self, pid, tag):
        self.shell("dumpsys", "battery", "reset", "-f")
        raw = self.shell("dumpsys", "battery")
        require(
            "UPDATES STOPPED" not in raw and "USB powered: true" in raw,
            "restoration_unverified",
        )
        self.shell("rm", "-f", "--", "/data/local/tmp/" + tag + ".armed")
        self.stop_owned(pid, tag, "-TERM")
        self.shell("rm", "-f", "--", "/data/local/tmp/" + tag + ".ready")

    def retain_trace(self, output, pid, config_path, trace_path, partial, sleep):
        self.stop_owned(pid, config_path, "-INT")
        size = self.shell("stat", "-c", "%s", trace_path)
        require(size.isdecimal() and 0 < int(size) <= MAX_TRACE, "trace_bounds")
        raw = self.adb("exec-out", "cat", trace_path, limit=MAX_TRACE)[1]
        require(len(raw) == int(size), "trace_changed")
        save(output, "trace.partial.pftrace" if partial else "trace.pftrace", raw)
        self.shell("rm", "--", config_path, trace_path)

    def probe(self, workload, inbound):
        args = [
            "ping",
            "-n",
            "-c",
            "3",
            "-W",
            "3",
            "-I",
            workload["peer_address"] if inbound else workload["phone_address"],
            workload["phone_address"] if inbound else workload["peer_address"],
        ]
        code, raw = (
            bounded_run(args, timeout=15, check=False)
            if inbound
            else self.adb("shell", shlex.join(args), timeout=15, check=False)
        )
        return {
            "success": code == 0,
            "exit_code": code,
            "summary": [
                line
                for line in raw.decode().splitlines()
                if "packets transmitted" in line
            ],
        }

    def _wake_ping(self, workload, inbound, count, wait, timeout):
        args = [
            "ping",
            "-n",
            "-c",
            str(count),
            "-W",
            str(wait),
            "-I",
            workload["peer_address"] if inbound else workload["phone_address"],
            workload["phone_address"] if inbound else workload["peer_address"],
        ]
        started = time.monotonic()
        code, raw = None, b""
        result = {
            "success": False,
            "packet_count": count,
            "wait_seconds": wait,
            "timeout_seconds": timeout,
            "started_monotonic_seconds": started,
        }
        try:
            code, raw = (
                bounded_run(args, timeout=timeout, check=False)
                if inbound
                else self.adb("shell", shlex.join(args), timeout=timeout, check=False)
            )
        except CommandFailure as error:
            code, raw = error.returncode, error.stdout
            result["error"] = str(error)
            result["stderr_base64"] = base64.b64encode(error.stderr).decode("ascii")
        except InterruptedCapture:
            result["error"] = "command_interrupted"
        except compare.InvalidWindow as error:
            result["error"] = str(error)
        except compare.MALFORMED_EVIDENCE:
            result["error"] = "probe_unavailable"
        result["elapsed_seconds"] = time.monotonic() - started
        result["exit_code"] = code
        result["stdout"] = raw.decode(errors="replace")
        result["stdout_base64"] = base64.b64encode(raw).decode("ascii")
        result["summary"] = [
            line
            for line in result["stdout"].splitlines()
            if "packets transmitted" in line
        ]
        delivered = re.findall(
            r"(?m)^([0-9]+) packets transmitted,\s*([0-9]+) (?:packets )?received,",
            result["stdout"],
        )
        result["success"] = (
            "error" not in result
            and code == 0
            and delivered == [(str(count), str(count))]
        )
        if not result["success"] and "error" not in result:
            result["error"] = "packet_delivery_failed"
        return result

    def wake_probe(self, workload, inbound):
        cold = self._wake_ping(
            workload,
            inbound,
            1,
            FIRST_WAKE_REPLY_WAIT_SECONDS,
            WAKE_PROBE_TIMEOUT_SECONDS,
        )
        # Never let later traffic rescue the first packet or obscure its failure.
        steady = (
            self._wake_ping(workload, inbound, 3, 3, STEADY_PROBE_TIMEOUT_SECONDS)
            if cold["success"]
            else None
        )
        final = steady if steady is not None else cold
        result = {
            "success": cold["success"] and steady is not None and steady["success"],
            "exit_code": final["exit_code"],
            "summary": final["summary"],
            "cold": cold,
            "steady": steady,
        }
        if "error" in final:
            result["error"] = final["error"]
        return result


def capture(directory, slot, device, sleep=time.sleep):
    require(
        directory.is_dir()
        and not directory.is_symlink()
        and directory.stat().st_uid == os.getuid()
        and directory.stat().st_mode & 0o077 == 0,
        "private_directory",
    )
    raw_plan = compare.read_file(directory / "plan.json", compare.MAX_JSON)
    plan = compare.parse_json(raw_plan)
    require(
        plan["collector_source_sha256"] == sha(Path(__file__).read_bytes()),
        "collector_changed",
    )
    names = plan["windows"]
    require(
        names == ["01-A", "02-B", "03-B", "04-A", "05-A", "06-B"] and slot in names,
        "plan_order",
    )
    index = names.index(slot)
    for previous in names[:index]:
        prior = compare.read_json(directory / previous / "result.json")
        require(
            prior["captured"] is True
            and prior["restoration_verified"] is True
            and prior.get("boundary_probes_passed") is True
            and not prior["cleanup_failures"],
            "previous_attempt_incomplete",
        )
    seconds = compare.integer(plan["window_seconds"], 1800, 7200, "window_duration")
    settle = compare.integer(plan["settle_seconds"], 300, 1800, "settling")
    raw_workload = compare.read_file(directory / "workload.json", compare.MAX_JSON)
    workload = compare.parse_json(raw_workload)
    validate_workload(workload)
    require(sha(raw_workload) == plan["workload_sha256"], "workload_changed")
    config = compare.read_file(directory / "trace.pbtxt", compare.MAX_JSON)
    require(
        config == trace_config(seconds) and sha(config) == plan["trace_config_sha256"],
        "trace_config_changed",
    )
    output = directory / slot
    output.mkdir(mode=0o700)
    tag = "p2p-vpn-battery-" + uuid.uuid4().hex
    config_path = "/data/misc/perfetto-configs/" + tag + ".pbtxt"
    trace_path = "/data/misc/perfetto-traces/" + tag + ".pftrace"
    fallback_pid = trace_pid = None
    identity = None
    metadata = None
    result = {
        "schema_version": 1,
        "captured": False,
        "restoration_verified": False,
        "goal_complete": False,
        "plan_sha256": sha(raw_plan),
        "collector_source_sha256": sha(Path(__file__).read_bytes()),
        "remote_config_path": config_path,
        "remote_trace_path": trace_path,
    }
    try:
        device.preflight(workload)
        identity, _ = device.identities(workload)
        expected = plan[
            "reference_apk" if compare.ORDER[index] == "reference" else "candidate_apk"
        ]
        require(identity["apk"] == expected, "wrong_apk")
        uid = device.uid()
        compare.decoder.report_type()
        save(output, "initial-identity.json", identity)
        save(
            output,
            "capture-identity.json",
            {
                "uid": uid,
                "arm": compare.ORDER[index],
                "identity": identity,
                "workload_sha256": sha(raw_workload),
                "window_seconds": seconds,
            },
        )
        probes = {
            key: False
            for key in (
                "inbound_before",
                "outbound_before",
                "inbound_after",
                "outbound_after",
            )
        }
        for inbound, key in ((True, "inbound_before"), (False, "outbound_before")):
            probe = device.wake_probe(workload, inbound)
            save(output, key + ".json", probe)
            probes[key] = probe["success"]
            if probe.get("error") == "command_interrupted":
                raise InterruptedCapture()
        require(
            probes["inbound_before"] and probes["outbound_before"],
            "initial_connectivity_failed",
        )
        fallback_seconds = settle + seconds + FALLBACK_ALLOWANCE_SECONDS
        fallback_pid = device.fallback(tag, fallback_seconds)
        save(
            output,
            "fallback.json",
            {"pid": fallback_pid, "tag": tag, "seconds": fallback_seconds},
        )
        device.begin_override(fallback_pid, tag)
        emit("settling", slot=slot, seconds=settle)
        began = time.monotonic()
        sleep(settle)
        actual_settle = int(time.monotonic() - began)
        # Tracing brackets both endpoint reports; it is not aligned after the first sample.
        device.adb("push", str(directory / "trace.pbtxt"), config_path)
        raw = device.adb(
            "shell",
            shlex.join(
                [
                    "perfetto",
                    "--background-wait",
                    "--txt",
                    "-c",
                    config_path,
                    "-o",
                    trace_path,
                    "--no-clobber",
                ]
            ),
            timeout=45,
        )[1]
        save(output, "trace-start.txt", raw)
        require(raw.strip().isdigit() and int(raw.strip()) > 1, "trace_pid")
        trace_pid = int(raw.strip())
        sleep(5)
        start_identity, start_evidence = device.identities(workload)
        require(start_identity == identity, "identity_changed")
        start_context = device.context(workload, start_evidence)
        save(output, "start-evidence.json", start_evidence)
        sample_started = time.monotonic()
        start = device.endpoint(output, "start", start_context, uid)
        save(output, "start-endpoint.json", start)
        emit("capturing", slot=slot, seconds=seconds)
        remaining = sample_started + seconds - time.monotonic()
        require(remaining > 0, "sample_deadline_missed")
        sleep(remaining)
        # End report precedes post-window status reads and both connectivity probes.
        end = device.endpoint(output, "end", None, uid)
        save(output, "end-endpoint-clock.json", end)
        end_identity, end_evidence = device.identities(workload)
        end["context"] = device.context(workload, end_evidence)
        save(output, "end-evidence.json", end_evidence)
        save(output, "end-endpoint.json", end)
        require(end_identity == identity, "identity_changed")
        metadata = {
            "schema_version": 1,
            "arm": compare.ORDER[index],
            "uid": uid,
            "settle_seconds": actual_settle,
            "window_seconds": seconds,
            "identity": {
                **identity,
                "workload": sha(raw_workload),
                "trace_config": sha(config),
            },
            "start": start,
            "end": end,
            "probes": probes,
        }
        sleep(3)
        device.retain_trace(output, trace_pid, config_path, trace_path, False, sleep)
        trace_pid = None
        for inbound, key in ((True, "inbound_after"), (False, "outbound_after")):
            try:
                probe = device.wake_probe(workload, inbound)
            except CommandFailure as error:
                probe = {
                    "success": False,
                    "exit_code": error.returncode,
                    "summary": [],
                    "error": str(error),
                    "stdout_base64": base64.b64encode(error.stdout).decode("ascii"),
                    "stderr_base64": base64.b64encode(error.stderr).decode("ascii"),
                }
            except compare.MALFORMED_EVIDENCE:
                probe = {
                    "success": False,
                    "exit_code": None,
                    "summary": [],
                    "error": "probe_unavailable",
                }
            save(output, key + ".json", probe)
            probes[key] = probe["success"]
            if probe.get("error") == "command_interrupted":
                raise InterruptedCapture()
        require(
            compare.read_file(directory / "plan.json", compare.MAX_JSON) == raw_plan,
            "plan_changed",
        )
        compare.validate_context(metadata)
        compare.report_window(
            compare.read_file(output / "start.pb", compare.decoder.MAX_REPORT_BYTES),
            compare.read_file(output / "end.pb", compare.decoder.MAX_REPORT_BYTES),
            metadata,
        )
        result["captured"] = True
        result["boundary_probes_passed"] = all(probes.values())
    except InterruptedCapture:
        result["error"] = "interrupted"
    except compare.InvalidWindow as error:
        result["error"] = str(error)
    except compare.MALFORMED_EVIDENCE:
        result["error"] = "capture_unavailable_or_malformed"
    finally:
        # A second interruption must not abort the bounded reporting restoration.
        handlers = {
            sig: signal.signal(sig, signal.SIG_IGN)
            for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGALRM)
        }
        signal.alarm(0)
        failures = []
        if metadata is not None:
            try:
                save(output, "window.json", metadata)
            except OSError:
                failures.append("window_metadata_save_failed")
        if fallback_pid:
            try:
                device.restore(fallback_pid, tag)
                result["restoration_verified"] = True
            except (compare.InvalidWindow, *compare.MALFORMED_EVIDENCE):
                failures.append("battery_restoration_unverified_fallback_retained")
        else:
            result["restoration_verified"] = True
        if trace_pid:
            try:
                device.retain_trace(
                    output, trace_pid, config_path, trace_path, True, sleep
                )
            except (compare.InvalidWindow, *compare.MALFORMED_EVIDENCE):
                failures.append("partial_trace_unverified")
        if identity is not None:
            try:
                restored_identity, _ = device.identities(workload)
                require(restored_identity == identity, "app_or_profile_changed")
            except (compare.InvalidWindow, *compare.MALFORMED_EVIDENCE):
                failures.append("app_or_profile_verification_failed")
        result["battery_reporting_restored"] = result["restoration_verified"]
        result["restoration_verified"] = result["restoration_verified"] and not failures
        result["cleanup_failures"] = failures
        try:
            save(output, "result.json", result)
            emit("finished", slot=slot, **result)
        finally:
            for sig, handler in handlers.items():
                signal.signal(sig, handler)
    return result


def interrupted(_signum, _frame):
    raise InterruptedCapture()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    initialize_parser = commands.add_parser("init")
    initialize_parser.add_argument("directory", type=Path)
    initialize_parser.add_argument("--reference-apk", type=Path, required=True)
    initialize_parser.add_argument("--candidate-apk", type=Path, required=True)
    initialize_parser.add_argument("--workload", type=Path, required=True)
    initialize_parser.add_argument("--window-seconds", type=int, default=1800)
    initialize_parser.add_argument("--settle-seconds", type=int, default=300)
    capture_parser = commands.add_parser("capture")
    capture_parser.add_argument("directory", type=Path)
    capture_parser.add_argument("--slot", required=True)
    capture_parser.add_argument("--serial", required=True)
    capture_parser.add_argument(
        "--simulate-on-battery", action="store_true", required=True
    )
    args = parser.parse_args()
    try:
        if args.command == "init":
            initialize(
                args.directory,
                args.reference_apk,
                args.candidate_apk,
                args.workload,
                args.window_seconds,
                args.settle_seconds,
            )
            emit("initialized")
            return 0
        device = Device(args.serial)
        with device_lock(args.serial):
            for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
                signal.signal(signum, interrupted)
            plan = compare.read_json(args.directory / "plan.json")
            duration = compare.integer(
                plan["window_seconds"], 1800, 7200, "window_duration"
            )
            settle = compare.integer(plan["settle_seconds"], 300, 1800, "settling")
            signal.alarm(duration + settle + WATCHDOG_ALLOWANCE_SECONDS)
            result = capture(args.directory, args.slot, device)
        return (
            0
            if result["captured"]
            and result["restoration_verified"]
            and result.get("boundary_probes_passed")
            and not result["cleanup_failures"]
            else 1
        )
    except compare.InvalidWindow as error:
        emit("failed", error=str(error))
    except (InterruptedCapture, *compare.MALFORMED_EVIDENCE):
        emit("failed", error="capture_unavailable_or_malformed")
    finally:
        signal.alarm(0)
    return 1


if __name__ == "__main__":
    sys.exit(main())

"""Analyze retained cellular battery windows; never control the phone."""

import argparse
import bisect
import csv
import hashlib
import importlib.util
import io
import json
import math
import os
import re
import resource
import signal
import stat
import statistics
import subprocess
import sys
import tempfile
import uuid
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "battery_decoder", Path(__file__).with_name("android-battery-decode.py")
)
decoder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(decoder)

ORDER = ("reference", "candidate", "candidate", "reference", "reference", "candidate")
IDENTITIES = (
    "device",
    "os_build",
    "apk",
    "profile",
    "peer_inventory",
    "workload",
    "trace_config",
)
POLICIES = (
    "accounting_mode",
    "physical_usb_powered",
    "power_save",
    "device_idle",
    "authorized_peers",
)
RAILS = ("power.rails.modem", "power.rails.radio.frontend")
MAX_JSON = 64 * 1024
MAX_TRACE = 64 * 1024 * 1024
MAX_PROCESSOR_OUTPUT = 2 * 1024 * 1024
MAX_PROCESSOR_MEMORY = 1024 * 1024 * 1024
MAX_SAMPLES = 20000


class InvalidWindow(ValueError):
    """Fixed, non-sensitive validation reason."""


class AnalysisDeadline(Exception):
    """Fatal deadline, never converted into an ordinary invalid-window retry."""


MALFORMED_EVIDENCE = (
    KeyError,
    TypeError,
    ValueError,
    OverflowError,
    OSError,
    RecursionError,
    decoder.DecodeError,
    subprocess.SubprocessError,
)


def require(condition, code):
    if not condition:
        raise InvalidWindow(code)


def integer(value, low, high, code):
    require(type(value) is int and low <= value <= high, code)
    return value


def digest(value):
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value), "identity")
    return value


def read_file(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= limit, "file_bounds")
        raw = stream.read(limit + 1)
    require(0 < len(raw) <= limit, "file_bounds")
    return raw


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_json_key")
        result[key] = value
    return result


def parse_json(raw):
    value = json.loads(raw, object_pairs_hook=unique_object)
    require(isinstance(value, dict), "json_shape")
    return value


def read_json(path):
    return parse_json(read_file(path, MAX_JSON))


def report_window(start_raw, end_raw, metadata):
    """Derive one accounting interval, independent of mutable wall-clock labels."""
    integer(metadata["schema_version"], 1, 1, "schema")
    uid = integer(metadata["uid"], 0, 2**31 - 1, "uid")
    reports = [decoder.decode(raw, uid) for raw in (start_raw, end_raw)]
    require(
        all(r["uid_attribution_available"] for r in reports), "attribution_unavailable"
    )
    require(
        reports[0]["scope"]["start_count"] == reports[1]["scope"]["start_count"],
        "clock_session",
    )
    origins = []
    boot_ids = []
    for key, report in zip(("start", "end"), reports):
        endpoint = metadata[key]
        require(
            isinstance(endpoint["boot_id"], str) and len(endpoint["boot_id"]) == 36,
            "boot_id",
        )
        boot_ids.append(str(uuid.UUID(endpoint["boot_id"])))
        before = integer(endpoint["before_uptime_ms"], 0, 2**53, "clock_bracket")
        after = integer(
            endpoint["after_uptime_ms"], before, before + 2000, "clock_bracket"
        )
        total = report["scope"]["total_realtime_ms"]
        origins.append((before - total - 10, after - total + 10))
    lower, upper = max(o[0] for o in origins), min(o[1] for o in origins)
    require(boot_ids[0] == boot_ids[1] and lower <= upper, "clock_session")
    scopes = [r["scope"] for r in reports]
    deltas = {
        key: scopes[1][key] - scopes[0][key]
        for key in decoder.SCOPE_FIELDS
        if key not in ("start_clock_time_ms", "start_count")
    }
    require(all(value >= 0 for value in deltas.values()), "clock_reversal")
    elapsed = deltas["total_realtime_ms"]
    planned = integer(metadata["window_seconds"], 1800, 7200, "window_duration") * 1000
    require(abs(elapsed - planned) <= 5000, "window_duration")
    require(abs(deltas["battery_realtime_ms"] - elapsed) <= 100, "battery_accounting")
    require(abs(deltas["screen_off_realtime_ms"] - elapsed) <= 100, "screen_wake")
    charge = reports[1]["uid_consumption_mah"] - reports[0]["uid_consumption_mah"]
    require(math.isfinite(charge) and charge >= 0, "charge_reversal")
    rate = charge * 3600000 / elapsed
    require(math.isfinite(rate), "energy_overflow")
    origin = (lower + upper) / 2
    return {
        "uid": uid,
        "elapsed_seconds": elapsed / 1000,
        "consumption_mah": charge,
        "app_mah_per_hour": rate,
        "start_boottime_ns": round((origin + scopes[0]["total_realtime_ms"]) * 1000000),
        "end_boottime_ns": round((origin + scopes[1]["total_realtime_ms"]) * 1000000),
        "clock_uncertainty_ms": (upper - lower) / 2,
        "raw_report_sha256": [r["raw_report_sha256"] for r in reports],
        "report_schema": [reports[0]["report_version"], reports[0]["parcel_version"]],
        "android_attribution_not_physical_discharge": True,
    }


def validate_context(metadata):
    require(
        set(metadata)
        == {
            "schema_version",
            "arm",
            "uid",
            "settle_seconds",
            "window_seconds",
            "identity",
            "start",
            "end",
            "probes",
        },
        "metadata_shape",
    )
    require(set(metadata["identity"]) == set(IDENTITIES), "identity")
    require(metadata["arm"] in ("reference", "candidate"), "arm")
    integer(metadata["settle_seconds"], 300, 7200, "settling")
    for key in IDENTITIES:
        digest(metadata["identity"][key])
    contexts = [metadata[key]["context"] for key in ("start", "end")]
    for key in ("start", "end"):
        require(
            set(metadata[key])
            == {"boot_id", "before_uptime_ms", "after_uptime_ms", "context"},
            "endpoint_shape",
        )
    for context in contexts:
        require(
            set(context)
            == {
                "app_pid",
                "runtime_generation",
                "screen_interactive",
                "wifi_enabled",
                "underlay_kind",
                "underlay_validated",
                "underlay_generation",
                "accounting_mode",
                "physical_usb_powered",
                "power_save",
                "device_idle",
                "thermal_status",
                "temperature_tenths_c",
                "rsrp_dbm",
                "authorized_peers",
                "payload_sent_packets",
                "payload_received_packets",
            },
            "context_shape",
        )
        integer(context["app_pid"], 1, 2**31 - 1, "process")
        integer(context["runtime_generation"], 1, 2**53, "process")
        require(context["screen_interactive"] is False, "screen_wake")
        require(
            context["wifi_enabled"] is False
            and context["underlay_kind"] == "cellular"
            and context["underlay_validated"] is True,
            "cellular_underlay",
        )
        digest(context["underlay_generation"])
        require(
            context["accounting_mode"] in ("simulated_on_battery", "discharging"),
            "power_policy",
        )
        for key in ("physical_usb_powered", "power_save"):
            require(type(context[key]) is bool, "power_policy")
        require(
            not (
                context["accounting_mode"] == "discharging"
                and context["physical_usb_powered"]
            ),
            "power_policy",
        )
        require(
            isinstance(context["device_idle"], str)
            and re.fullmatch(r"[A-Z_]{1,32}", context["device_idle"]),
            "power_policy",
        )
        integer(context["thermal_status"], 0, 0, "thermal")
        integer(context["temperature_tenths_c"], 180, 400, "temperature")
        integer(context["rsrp_dbm"], -120, -40, "signal")
        integer(context["authorized_peers"], 1, 128, "peer_inventory")
        for key in ("payload_sent_packets", "payload_received_packets"):
            integer(context[key], 0, 2**64 - 1, "payload_counter")
    a, b = contexts
    for key in (*POLICIES, "app_pid", "runtime_generation", "underlay_generation"):
        require(a[key] == b[key], "context_changed")
    for key in ("payload_sent_packets", "payload_received_packets"):
        require(a[key] == b[key], "non_idle_payload")
    require(
        abs(a["temperature_tenths_c"] - b["temperature_tenths_c"]) <= 20,
        "temperature_changed",
    )
    require(abs(a["rsrp_dbm"] - b["rsrp_dbm"]) <= 10, "signal_changed")
    probes = metadata["probes"]
    require(
        set(probes)
        == {"inbound_before", "outbound_before", "inbound_after", "outbound_after"},
        "probe_evidence",
    )
    for key in ("inbound_before", "outbound_before", "inbound_after", "outbound_after"):
        require(type(probes[key]) is bool, "probe_evidence")
    return all(
        probes[key]
        for key in (
            "inbound_before",
            "outbound_before",
            "inbound_after",
            "outbound_after",
        )
    )


def interpolate(samples, when):
    times = [sample[0] for sample in samples]
    position = bisect.bisect_left(times, when)
    require(position < len(times) and when >= times[0], "rail_coverage")
    if times[position] == when:
        return samples[position][1]
    left, right = samples[position - 1], samples[position]
    return left[1] + (right[1] - left[1]) * (when - left[0]) / (right[0] - left[0])


def counter_bound(samples, when, upper):
    times = [sample[0] for sample in samples]
    require(times[0] <= when <= times[-1], "rail_coverage")
    index = (
        bisect.bisect_left(times, when)
        if upper
        else bisect.bisect_right(times, when) - 1
    )
    return samples[index][1]


def integrate_rails(rows, start_ns, end_ns, uncertainty_ns):
    require(0 < len(rows) <= MAX_SAMPLES, "rail_samples")
    tracks = {}
    for row in rows:
        name, track = row["name"], int(row["track_id"])
        require(name in RAILS and track >= 0, "rail_track")
        if name not in tracks:
            tracks[name] = (track, [])
        require(tracks[name][0] == track, "ambiguous_rail_track")
        timestamp = int(row["ts"])
        value = float(row["value"])
        require(timestamp >= 0 and math.isfinite(value) and value >= 0, "rail_counter")
        samples = tracks[name][1]
        if samples:
            require(0 < timestamp - samples[-1][0] <= 3000000000, "rail_gap")
            require(value >= samples[-1][1], "rail_reset")
        samples.append((timestamp, value))
    require(set(tracks) == set(RAILS), "missing_rail")
    result = []
    for name in RAILS:
        track, samples = tracks[name]
        require(len(samples) >= 2, "rail_samples")
        energy = interpolate(samples, end_ns) - interpolate(samples, start_ns)
        # Between counter samples a burst can occur anywhere, not linearly in time.
        low = max(
            0,
            counter_bound(samples, end_ns - uncertainty_ns, False)
            - counter_bound(samples, start_ns + uncertainty_ns, True),
        )
        high = counter_bound(samples, end_ns + uncertainty_ns, True) - counter_bound(
            samples, start_ns - uncertainty_ns, False
        )
        power = energy * 1000000 / (end_ns - start_ns)
        require(
            all(math.isfinite(value) for value in (energy, low, high, power)),
            "energy_overflow",
        )
        require(0 <= low <= energy <= high, "rail_interval")
        result.append(
            {
                "name": name,
                "track_id": track,
                "samples": len(samples),
                "energy_joules": energy / 1000000,
                "energy_joules_bounds": [low / 1000000, high / 1000000],
                "mean_mw": power,
            }
        )
    return result


def processor_limit():
    resource.setrlimit(
        resource.RLIMIT_FSIZE, (MAX_PROCESSOR_OUTPUT, MAX_PROCESSOR_OUTPUT)
    )
    resource.setrlimit(resource.RLIMIT_AS, (MAX_PROCESSOR_MEMORY, MAX_PROCESSOR_MEMORY))
    resource.setrlimit(resource.RLIMIT_CPU, (40, 40))


def trace_query(processor, trace, sql):
    # Regular-file stdout bounds child output without buffering unbounded pipe data.
    with tempfile.TemporaryFile() as output:
        result = subprocess.run(
            [str(processor), "query", "--quiet", "--format", "csv", str(trace), sql],
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.DEVNULL,
            timeout=45,
            preexec_fn=processor_limit,
            check=False,
        )
        require(result.returncode == 0, "trace_processor")
        output.seek(0)
        raw = output.read(MAX_PROCESSOR_OUTPUT + 1)
    require(len(raw) <= MAX_PROCESSOR_OUTPUT, "trace_output_bounds")
    try:
        rows = []
        for row in csv.DictReader(io.StringIO(raw.decode("utf-8"))):
            require(len(rows) <= MAX_SAMPLES, "trace_output_rows")
            rows.append(row)
        return rows
    except csv.Error as error:
        raise InvalidWindow("trace_csv") from error


def analyze_trace(processor, trace, interval):
    trace_raw = read_file(trace, MAX_TRACE)
    # Query one immutable private copy, so the recorded digest covers every query.
    with tempfile.TemporaryDirectory(prefix="p2p-vpn-rails-") as temporary:
        staged = Path(temporary) / "trace.pftrace"
        with staged.open("xb") as stream:
            os.fchmod(stream.fileno(), 0o600)
            stream.write(trace_raw)
        errors = trace_query(
            processor,
            staged,
            "SELECT count(*) AS n FROM stats WHERE value>0 AND severity IN ('error','data_loss')",
        )
        require(len(errors) == 1 and errors[0]["n"] == "0", "trace_loss")
        clocks = trace_query(
            processor,
            staged,
            "SELECT ts,clock_value FROM clock_snapshot WHERE clock_id=6 LIMIT 64",
        )
        require(
            clocks and all(int(c["ts"]) == int(c["clock_value"]) for c in clocks),
            "trace_clock",
        )
        rows = trace_query(
            processor,
            staged,
            "SELECT t.name,t.id AS track_id,c.ts,c.value FROM counter c JOIN counter_track t ON t.id=c.track_id WHERE t.type='power_rails' AND t.name IN ('power.rails.modem','power.rails.radio.frontend') ORDER BY t.id,c.ts LIMIT 20001",
        )
    return {
        "sha256": hashlib.sha256(trace_raw).hexdigest(),
        "rails": integrate_rails(
            rows,
            interval["start_boottime_ns"],
            interval["end_boottime_ns"],
            round(interval["clock_uncertainty_ms"] * 1000000),
        ),
    }


def analyze_window(directory, processor):
    require(directory.is_dir() and not directory.is_symlink(), "bundle_directory")
    raw_metadata = read_file(directory / "window.json", MAX_JSON)
    metadata = parse_json(raw_metadata)
    functionality_passed = validate_context(metadata)
    interval = report_window(
        read_file(directory / "start.pb", decoder.MAX_REPORT_BYTES),
        read_file(directory / "end.pb", decoder.MAX_REPORT_BYTES),
        metadata,
    )
    trace = analyze_trace(processor, directory / "trace.pftrace", interval)
    collector_errors = []
    collector_digest = None
    collector_file = directory / "result.json"
    marker = directory / "capture-identity.json"
    if collector_file.exists() or collector_file.is_symlink():
        raw_collector = read_file(collector_file, MAX_JSON)
        collector = parse_json(raw_collector)
        integer(collector["schema_version"], 1, 1, "collector_schema")
        collector_digest = hashlib.sha256(raw_collector).hexdigest()
        for key in ("captured", "restoration_verified"):
            if collector.get(key) is not True:
                collector_errors.append(key + "_not_verified")
        if collector.get("cleanup_failures") or collector.get("error"):
            collector_errors.append("collector_reported_failure")
    elif marker.exists() or marker.is_symlink():
        collector_errors.append("collector_attempt_incomplete")
    result = {
        "schema_version": 1,
        "kind": "android_cellular_window_analysis",
        "valid": not collector_errors,
        "metadata": metadata,
        "window_metadata_sha256": hashlib.sha256(raw_metadata).hexdigest(),
        "accounting": interval,
        "trace": trace,
        "functionality_passed": functionality_passed,
        "goal_complete": False,
    }
    if collector_errors:
        result.update(
            {
                "error": "collector_attempt_rejected",
                "collector_errors": collector_errors,
            }
        )
    if collector_digest:
        result["collector_result_sha256"] = collector_digest
    return result


def summarize(plan, windows):
    integer(plan["schema_version"], 1, 1, "schema")
    require(len(windows) == len(ORDER), "plan")
    reference, candidate = digest(plan["reference_apk"]), digest(plan["candidate_apk"])
    require(reference != candidate, "same_apk")
    duration = integer(plan["window_seconds"], 1800, 7200, "window_duration")
    require(all(w.get("valid") is True for w in windows), "invalid_window")
    for arm, window in zip(ORDER, windows):
        metadata = window["metadata"]
        require(
            metadata["arm"] == arm and metadata["window_seconds"] == duration,
            "plan_order",
        )
        require(
            metadata["identity"]["apk"]
            == (reference if arm == "reference" else candidate),
            "apk_mismatch",
        )
    first = windows[0]
    seen_reports, seen_traces = set(), set()
    previous_end = None
    for window in windows:
        accounting = window["accounting"]
        reports = accounting["raw_report_sha256"]
        require(
            len(set(reports)) == 2 and not seen_reports.intersection(reports),
            "reused_evidence",
        )
        require(window["trace"]["sha256"] not in seen_traces, "reused_evidence")
        seen_reports.update(reports)
        seen_traces.add(window["trace"]["sha256"])
        require(
            window["metadata"]["start"]["boot_id"]
            == first["metadata"]["start"]["boot_id"],
            "series_boot_changed",
        )
        uncertainty = accounting["clock_uncertainty_ms"] * 1000000
        require(
            previous_end is None
            or accounting["start_boottime_ns"] - uncertainty > previous_end,
            "overlapping_windows",
        )
        previous_end = accounting["end_boottime_ns"] + uncertainty
    for window in windows[1:]:
        for key in IDENTITIES:
            if key != "apk":
                require(
                    first["metadata"]["identity"][key]
                    == window["metadata"]["identity"][key],
                    "unmatched_identity",
                )
        require(
            first["accounting"]["uid"] == window["accounting"]["uid"]
            and first["accounting"]["report_schema"]
            == window["accounting"]["report_schema"],
            "unmatched_accounting",
        )
        require(
            first["metadata"]["settle_seconds"] == window["metadata"]["settle_seconds"],
            "unmatched_settling",
        )
        for key in POLICIES:
            require(
                first["metadata"]["start"]["context"][key]
                == window["metadata"]["start"]["context"][key],
                "unmatched_policy",
            )
    pairs = []
    for index in (0, 2, 4):
        a, b = windows[index : index + 2]
        baseline, trial = (a, b) if a["metadata"]["arm"] == "reference" else (b, a)
        contexts = [
            w["metadata"][end]["context"]
            for w in (baseline, trial)
            for end in ("start", "end")
        ]
        require(
            max(c["rsrp_dbm"] for c in contexts) - min(c["rsrp_dbm"] for c in contexts)
            <= 10,
            "unmatched_signal",
        )
        require(
            max(c["temperature_tenths_c"] for c in contexts)
            - min(c["temperature_tenths_c"] for c in contexts)
            <= 20,
            "unmatched_temperature",
        )
        rate = baseline["accounting"]["app_mah_per_hour"]
        require(rate > 0, "unresolved_baseline")
        baseline_rails, trial_rails = (
            baseline["trace"]["rails"],
            trial["trace"]["rails"],
        )
        ratio = trial["accounting"]["app_mah_per_hour"] / rate
        require(math.isfinite(ratio), "energy_overflow")
        rail_ratio = (
            sum(r["mean_mw"] for r in trial_rails)
            / sum(r["mean_mw"] for r in baseline_rails)
            if sum(r["mean_mw"] for r in baseline_rails) > 0
            else None
        )

        # Use power bounds rather than assuming slightly different window lengths match.
        def power_bound(window, side, rail_name=None):
            return (
                sum(
                    r["energy_joules_bounds"][side]
                    for r in window["trace"]["rails"]
                    if rail_name is None or r["name"] == rail_name
                )
                / window["accounting"]["elapsed_seconds"]
            )

        pairs.append(
            {
                "app_ratio": ratio,
                "modem_and_frontend_power_ratio": rail_ratio,
                "modem_reduction_above_sampling_and_clock_uncertainty": power_bound(
                    trial, 1, "power.rails.modem"
                )
                < power_bound(baseline, 0, "power.rails.modem"),
                "combined_rail_reduction_above_sampling_and_clock_uncertainty": power_bound(
                    trial, 1
                )
                < power_bound(baseline, 0),
            }
        )
    ratios = [p["app_ratio"] for p in pairs]
    functional = all(w["functionality_passed"] for w in windows)
    return {
        "schema_version": 1,
        "kind": "android_cellular_paired_analysis",
        "pairs": pairs,
        "app_ratio_median": statistics.median(ratios),
        "app_ratio_min": min(ratios),
        "app_ratio_max": max(ratios),
        "app_ratio_sample_stdev": statistics.stdev(ratios),
        "functionality_passed": functional,
        "energy_criterion_observed": functional
        and all(r <= 0.10 for r in ratios)
        and all(
            p["modem_reduction_above_sampling_and_clock_uncertainty"] for p in pairs
        )
        and all(
            p["combined_rail_reduction_above_sampling_and_clock_uncertainty"]
            for p in pairs
        ),
        "goal_complete": False,
        "limitations": [
            "Android attribution, not whole-phone battery life",
            "Endpoint context and probes require retained collector evidence",
            "Does not certify bilateral dormancy or the full recovery test matrix",
        ],
    }


def safe_window(directory, processor):
    try:
        return analyze_window(directory, processor)
    except InvalidWindow as error:
        return {"valid": False, "error": str(error), "goal_complete": False}
    except MALFORMED_EVIDENCE:
        return {
            "valid": False,
            "error": "unavailable_or_malformed_evidence",
            "goal_complete": False,
        }


def timed_out(_signum, _frame):
    raise AnalysisDeadline()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("window", "series"))
    parser.add_argument("directory", type=Path)
    parser.add_argument(
        "--trace-processor",
        type=Path,
        required=True,
        help="Explicit trusted Perfetto trace_processor_shell executable",
    )
    args = parser.parse_args()
    result = {"valid": False, "goal_complete": False}
    signal.signal(signal.SIGALRM, timed_out)
    signal.alarm(1800)
    try:
        processor = args.trace_processor.resolve(strict=True)
        tool_hash = hashlib.sha256(read_file(processor, 256 * 1024 * 1024)).hexdigest()
        if args.mode == "window":
            result = safe_window(args.directory, processor)
        else:
            raw_plan = read_file(args.directory / "plan.json", MAX_JSON)
            plan = parse_json(raw_plan)
            names = plan["windows"]
            require(
                isinstance(names, list) and len(names) == 6 and len(set(names)) == 6,
                "plan_windows",
            )
            require(
                all(
                    isinstance(n, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,64}", n)
                    for n in names
                ),
                "plan_windows",
            )
            windows = [
                {"valid": False, "error": "not_analyzed", "goal_complete": False}
                for _ in names
            ]
            result["windows"] = windows
            result["plan_sha256"] = hashlib.sha256(raw_plan).hexdigest()
            for index, name in enumerate(names):
                try:
                    windows[index] = safe_window(args.directory / name, processor)
                except AnalysisDeadline:
                    windows[index] = {
                        "valid": False,
                        "error": "analysis_timeout",
                        "goal_complete": False,
                    }
                    raise
            result.update(summarize(plan, windows))
            result["valid"] = True
        result["trace_processor_sha256"] = tool_hash
        for key, source in (
            ("analyzer_source_sha256", Path(__file__)),
            ("decoder_source_sha256", Path(decoder.__file__)),
            ("decoder_projection_sha256", decoder.SCHEMA),
        ):
            result[key] = hashlib.sha256(read_file(source, MAX_JSON)).hexdigest()
    except AnalysisDeadline:
        result.update(
            {"valid": False, "error": "analysis_timeout", "goal_complete": False}
        )
    except InvalidWindow as error:
        result.update({"valid": False, "error": str(error), "goal_complete": False})
    except MALFORMED_EVIDENCE:
        result.update(
            {
                "valid": False,
                "error": "unavailable_or_malformed_evidence",
                "goal_complete": False,
            }
        )
    finally:
        signal.alarm(0)
    print(json.dumps(result, allow_nan=False, sort_keys=True))
    return 0 if result["valid"] and result.get("functionality_passed", False) else 1


if __name__ == "__main__":
    sys.exit(main())

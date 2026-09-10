#!/bin/sh
# Run through the debug application's run-as identity on an owned emulator.
set -eu
set -f

unsigned() {
  case "$1" in '' | *[!0-9]*) return 1 ;; esac
}

read_stat() {
  IFS= read -r stat <"$proc/$pid/stat" || return 1
  # comm may contain spaces and closing parentheses; use the final delimiter.
  fields=${stat##*) }
  [ "$fields" != "$stat" ] || return 1
  # Intentional word splitting of numeric kernel fields; globbing is disabled.
  # shellcheck disable=SC2086
  set -- $fields
  [ "$#" -ge 22 ] || return 1
  shift 11
  user_ticks=$1 system_ticks=$2
  shift 6
  threads=$1
  shift 2
  start_ticks=$1
  for value in "$user_ticks" "$system_ticks" "$threads" "$start_ticks"; do
    unsigned "$value" || return 1
  done
}

sample() {
  read -r started _ <"$proc/uptime" || return 1
  read_stat || return 1
  identity=$start_ticks
  [ "$identity" = "$expected_start" ] || return 1
  before_user=$user_ticks before_system=$system_ticks before_threads=$threads
  rss=null voluntary=null involuntary=null
  if [ -r "$proc/$pid/status" ]; then
    while read -r key value _; do
      case "$key" in
        VmRSS:) unsigned "$value" && rss=$value ;;
        voluntary_ctxt_switches:) unsigned "$value" && voluntary=$value ;;
        nonvoluntary_ctxt_switches:) unsigned "$value" && involuntary=$value ;;
      esac
    done <"$proc/$pid/status"
  fi
  fds=null
  if [ -r "$proc/$pid/fd" ] && [ -x "$proc/$pid/fd" ]; then
    fds=0
    set +f
    for fd in "$proc/$pid/fd/"*; do
      [ -L "$fd" ] && fds=$((fds + 1))
    done
    set -f
  fi
  read_stat || return 1
  [ "$start_ticks" = "$identity" ] || return 1
  read -r finished _ <"$proc/uptime" || return 1
  printf '{"schema_version":1,"pid":%s,"start_ticks":%s,"started_uptime_seconds":%s,"finished_uptime_seconds":%s,"user_ticks":%s,"system_ticks":%s,"os_threads":%s,"rss_kib":%s,"fds":%s,"leader_voluntary_context_switches":%s,"leader_involuntary_context_switches":%s}\n' \
    "$pid" "$identity" "$started" "$finished" "$before_user" "$before_system" \
    "$before_threads" "$rss" "$fds" "$voluntary" "$involuntary"
}

if [ "$#" -ne 2 ] || ! unsigned "$1" || ! unsigned "$2"; then
  printf 'Usage: sh android-process-sample.sh PID SAMPLE_COUNT (1..900)\n' >&2
  exit 2
fi
pid=$1 count=$2
case "$pid:$count" in 0* | *:0*) exit 2 ;; esac
[ "$pid" -gt 0 ] && [ "$count" -ge 1 ] && [ "$count" -le 900 ] || exit 2
proc=${P2P_VPN_SAMPLE_PROC_ROOT:-/proc}
read_stat || exit 1
expected_start=$start_ticks
index=0
while [ "$index" -lt "$count" ]; do
  if ! sample; then
    printf 'Process sample unavailable or process identity changed.\n' >&2
    exit 1
  fi
  index=$((index + 1))
  [ "$index" -eq "$count" ] || sleep 1
done

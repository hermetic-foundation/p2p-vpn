#!/bin/sh
# Run from a privileged ADB shell on an owned root-capable emulator.
set -eu
set -f

unsigned() {
  case "$1" in '' | *[!0-9]*) return 1 ;; esac
}

read_stat() {
  IFS= read -r stat <"${1:-$proc/$pid/stat}" || return 1
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

thread_sample() (
  task=$1 tid=${1##*/}
  read_stat "$task/stat" || exit 1
  task_start=$start_ticks task_user=$user_ticks task_system=$system_ticks
  task_voluntary=null task_involuntary=null
  [ -r "$task/status" ] || exit 1
  while read -r key value _; do
    case "$key" in
      voluntary_ctxt_switches:) unsigned "$value" && task_voluntary=$value ;;
      nonvoluntary_ctxt_switches:) unsigned "$value" && task_involuntary=$value ;;
    esac
  done <"$task/status"
  read_stat "$task/stat" || exit 1
  [ "$start_ticks" = "$task_start" ] || exit 1
  printf '{"tid":%s,"start_ticks":%s,"user_ticks":%s,"system_ticks":%s,"voluntary_context_switches":%s,"involuntary_context_switches":%s}' \
    "$tid" "$task_start" "$task_user" "$task_system" "$task_voluntary" "$task_involuntary"
)

thread_scan() {
  [ -r "$proc/$pid/task" ] && [ -x "$proc/$pid/task" ] || return 1
  listed=0 observed=0 task_rows="" separator=""
  set +f
  for task in "$proc/$pid/task/"*; do
    tid=${task##*/}
    unsigned "$tid" || continue
    listed=$((listed + 1))
    [ "$listed" -le 256 ] || {
      set -f
      return 1
    }
    if task_row=$(thread_sample "$task" 2>/dev/null); then
      observed=$((observed + 1))
      task_rows="$task_rows$separator$task_row"
      separator=,
    fi
  done
  set -f
  [ "$listed" -gt 0 ] || return 1
  thread_json=",\"thread_scan\":{\"listed\":$listed,\"observed\":$observed,\"skipped\":$((listed - observed)),\"threads\":[$task_rows]}"
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
  thread_json=""
  if [ "$include_threads" = true ]; then
    thread_scan || return 1
  fi
  read_stat || return 1
  [ "$start_ticks" = "$identity" ] || return 1
  read -r finished _ <"$proc/uptime" || return 1
  printf '{"schema_version":1,"pid":%s,"start_ticks":%s,"started_uptime_seconds":%s,"finished_uptime_seconds":%s,"user_ticks":%s,"system_ticks":%s,"os_threads":%s,"rss_kib":%s,"fds":%s,"leader_voluntary_context_switches":%s,"leader_involuntary_context_switches":%s%s}\n' \
    "$pid" "$identity" "$started" "$finished" "$before_user" "$before_system" \
    "$before_threads" "$rss" "$fds" "$voluntary" "$involuntary" "$thread_json"
}

if { [ "$#" -ne 2 ] && [ "$#" -ne 3 ]; } || ! unsigned "$1" || ! unsigned "$2"; then
  printf 'Usage: sh android-process-sample.sh PID SAMPLE_COUNT (1..900) [--threads]\n' >&2
  exit 2
fi
include_threads=false
if [ "$#" -eq 3 ]; then
  [ "$3" = --threads ] || exit 2
  include_threads=true
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

#!/usr/bin/env bash
set -euo pipefail

count=1
previous=""
for argument in "$@"; do
  if [[ "$previous" == -c ]]; then
    count="$argument"
  fi
  previous="$argument"
done
printf '%s packets transmitted, %s received, 0%% packet loss\n' "$count" "$count"

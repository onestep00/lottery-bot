#!/usr/bin/env bash
set -euo pipefail
umask 077
project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd -- "$project_dir"
case "${1:-}" in buy|check) ;; *) echo 'Usage: run-job.sh buy|check' >&2; exit 2;; esac
mkdir -p .logs
exec >> ".logs/$(date +%Y-%m).log" 2>&1
printf '\n[%s] starting %s\n' "$(date -Is)" "$1"
exec /usr/bin/timeout --signal=TERM --kill-after=30s 15m "$project_dir/bin/lottery-bot" "$1" --env "$project_dir/.env"

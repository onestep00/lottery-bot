#!/usr/bin/env bash
set -euo pipefail
umask 077
crontab_tmp=$(mktemp)
trap 'rm -f "$crontab_tmp"' EXIT
crontab -l > "$crontab_tmp"
awk '
 $0 == "# BEGIN lottery-bot local schedule" { skip=1; next }
 $0 == "# END lottery-bot local schedule" { skip=0; next }
 !skip { print }
' "$crontab_tmp" | crontab -
echo 'lottery-bot cron 제거 완료. 구매 상태와 로그는 보존했습니다.'

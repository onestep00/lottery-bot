#!/usr/bin/env bash
set -euo pipefail
umask 077
project_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd -- "$project_dir"
command -v crontab >/dev/null
# Debian cron uses the system timezone, not CRON_TZ. Reject a mismatched host.
if [[ $(date +%z) != +0900 ]]; then
    echo '시스템 시간대를 Asia/Seoul로 설정한 뒤 설치하세요. 기존 일정은 변경하지 않았습니다.' >&2
    exit 1
fi
[[ -f .env ]] || { echo '.env 설정이 필요합니다 (.env.sample 참조)' >&2; exit 1; }
chmod 600 .env
cargo build --release --locked
mkdir -p bin .state .logs
chmod 700 .state .logs
install -m 700 target/release/lottery-bot bin/lottery-bot.new
mv -f bin/lottery-bot.new bin/lottery-bot
./bin/lottery-bot doctor --env "$project_dir/.env"
crontab_tmp=$(mktemp)
trap 'rm -f "$crontab_tmp"' EXIT
# Back up, preserve other tasks, and replace only this project's managed block.
if ! crontab -l > "$crontab_tmp" 2> .state/crontab-read.err; then
    if ! LC_ALL=C grep -q 'no crontab for' .state/crontab-read.err; then
        cat .state/crontab-read.err >&2
        exit 1
    fi
    : > "$crontab_tmp"
fi
cp "$crontab_tmp" ".state/crontab.before.$(date +%Y%m%d%H%M%S)"
awk '
 $0 == "# BEGIN lottery-bot local schedule" { skip=1; next }
 $0 == "# END lottery-bot local schedule" { skip=0; next }
 !skip { print }
' "$crontab_tmp" > "${crontab_tmp}.new"
mv "${crontab_tmp}.new" "$crontab_tmp"
printf -v runner '%q' "$project_dir/scripts/run-job.sh"
# crontab treats percent specially, even inside shell quotes.
runner=${runner//%/\\%}
{
    printf '\n# BEGIN lottery-bot local schedule\n'
    printf '# Host timezone must remain Asia/Seoul (UTC+09:00). No catch-up/retry jobs.\n'
    printf '0 19 * * 1 /bin/bash %s buy\n' "$runner"
    printf '0 22 * * 6 /bin/bash %s check\n' "$runner"
    printf '# END lottery-bot local schedule\n'
} >> "$crontab_tmp"
crontab "$crontab_tmp"
echo 'cron 등록 완료: 월요일 19:00 구매 / 토요일 22:00 확인 (KST)'

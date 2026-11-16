#!/usr/bin/env bash
# バックアップを取る見本スクリプト
set -euo pipefail

BACKUP_DIR="${HOME}/backup"
count=0
declare -r LIMIT=10

log() {
    printf '[%s] %s\n' "$(date +%H:%M:%S)" "$*" >&2
}

function cleanup {
    rm -f /tmp/work.$$ # 一時ファイルを消す
}
trap cleanup EXIT

for f in *.txt "$@"; do
    if [[ -f $f && $count -lt $LIMIT ]]; then
        cp -- "$f" "$BACKUP_DIR/" && count=$((count + 1))
        log "copied $f (#$count)"
    elif [ ! -e "$f" ]; then
        echo 'skip: $f is missing'
        continue
    else
        echo "tab\there: \"$f\" `basename $f`"
    fi
done

case "$1" in
    start) echo $'ready\n' ;;
    -h|--help) exit 0 ;;
    *) exit 1 ;;
esac

cat <<EOF > "$BACKUP_DIR/report.txt"
total: $count of ${LIMIT}
done at $(date)
EOF
echo "finished" | grep -c fin && exit 0

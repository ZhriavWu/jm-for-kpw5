#!/bin/bash
# 扫 Kindle 日志：只看最近两周，找值得处理的东西。
set -u
D=/mnt/k/kindle-manga-logs
[ -d "$D" ] || D=/mnt/k/Kindle/kindle-manga-logs
ls "$D" >/dev/null 2>&1 || { echo "找不到日志目录：$D"; exit 1; }

echo "===== power：挂起成功的 vs 失败的 ====="
for f in "$D"/*.log; do
    n=$(basename "$f")
    # 只看 10-20 之后的
    case "$n" in
        2026100*|2026101[0-9]*|2026102[0-9]*|202610[3-9]*) ;;
        *) continue ;;
    esac
    ok=$(grep -ac '已唤醒\|唤醒' "$f" 2>/dev/null || echo 0)
    bad=$(grep -ac '挂起失败\|首次挂起被拒' "$f" 2>/dev/null || echo 0)
    if [ "$bad" -gt 0 ] || [ "$ok" -gt 0 ]; then
        echo "  $n  唤醒迹象=$ok  挂起失败=$bad"
    fi
done

echo
echo "===== 最近 5 个日志里的所有 WARN ====="
for f in $(ls -1t "$D"/*.log | head -5); do
    echo "--- $(basename "$f") ---"
    grep -a '\[WARN\]' "$f" | sed 's/^/    /' | head -8
done

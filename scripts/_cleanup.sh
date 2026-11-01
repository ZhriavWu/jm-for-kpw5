#!/bin/bash
# 收工清理：关掉本次会话起的应用 / 编译 / 测试进程。
#
# ⚠️ **不动 Xvfb**：这台机器上的两个 Xvfb（pid 784 / 1091）卡在 D 状态，
#    README 记过「kill -9 也杀不掉」，碰它会挂住。关机时会一并结束。
#
# ⚠️ 用 `pkill -x`（精确进程名）。**绝不用 `pkill -f`** ——
#    命令行里含那个字符串时会把自己的 shell 一起杀掉（README 5.1 记过）。
set -u

echo "=== 关闭前 ==="
pgrep -a kindle-manga || echo "  kindle-manga: 无"
pgrep -af _slow_proxy.py || echo "  慢代理: 无"
pgrep -a cargo || echo "  cargo: 无"

echo
echo "=== 关应用 ==="
pkill -x kindle-manga 2>/dev/null && echo "  已关 kindle-manga" || echo "  kindle-manga 本来就没跑"

echo "=== 关慢代理（本轮测试用的）==="
# 这个用 -f 是安全的：模式字符串不含在本脚本名里
pkill -f '_slow_proxy\.py' 2>/dev/null && echo "  已关慢代理" || echo "  慢代理本来就没跑"

echo "=== 关编译进程 ==="
pkill -x rustc 2>/dev/null && echo "  已停 rustc" || echo "  无 rustc"
pkill -x cargo 2>/dev/null && echo "  已停 cargo" || echo "  无 cargo"

sleep 1
echo
echo "=== 关闭后复查 ==="
for p in kindle-manga rustc cargo; do
    if pgrep -x "$p" > /dev/null 2>&1; then
        echo "  ⚠️  $p 还在: $(pgrep -x "$p" | tr '\n' ' ')"
    else
        echo "  OK   $p 已停"
    fi
done
if pgrep -f '_slow_proxy\.py' > /dev/null 2>&1; then
    echo "  ⚠️  慢代理还在"
else
    echo "  OK   慢代理已停"
fi

echo
echo "=== 保留不动（关机时一并结束）==="
echo "  Xvfb: $(pgrep -x Xvfb | tr '\n' ' ')(D 状态，杀不掉，按 README 不碰)"

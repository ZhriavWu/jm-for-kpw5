#!/bin/bash
# 一次性：验证「加载期间点击不再排队」。
#
# 真机上取图/搜索要好几秒，而这台机器上服务端 1 毫秒就答了，
# 所以中间加一个**慢代理**把 /api/search 拖慢 10 秒，模拟真机。
#
# 判据：点「搜索」之后立刻点底栏「历史」，量「历史页多久出现」——
#   * 修好了 → 1 秒内（搜索还在飞）
#   * 没修好 → 10 秒以上（点击排在搜索后面，等它跑完才被处理）
set -u

PROJ=$(cd "$(dirname "$0")/.." && pwd)
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
PROXY_PORT=8009
SEARCH_DELAY=10
LOG=/tmp/queue_app.log

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga
cd "$PROJ" || exit 1

echo "===== 1) 强制重编 ====="
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^error' /tmp/fb.log | head -10 || true

GW=$(ip route show default | awk '{print $3}')
echo "上游（Windows 上的服务端）= http://$GW:8001"

echo
echo "===== 2) 起慢代理（search +${SEARCH_DELAY}s，cover +3s）====="
pkill -f _slow_proxy.py 2>/dev/null
sleep 0.5
nohup python3 "$PROJ/scripts/_slow_proxy.py" "$GW" 8001 "$PROXY_PORT" "$SEARCH_DELAY" 3 > /tmp/proxy.log 2>&1 &
sleep 1.5
curl -s -m 5 -o /dev/null "http://127.0.0.1:$PROXY_PORT/api/info" || { echo "!! 代理没起来"; cat /tmp/proxy.log; exit 1; }
echo "代理就绪"
printf 'http://127.0.0.1:%d' "$PROXY_PORT" > "$OUT/server.txt"
rm -f "$OUT/favorites.json"

echo
echo "===== 3) 起 GUI（指向慢代理）====="
export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5
pkill -x kindle-manga 2>/dev/null && sleep 1
: > "$LOG"
nohup "$BIN" --server "http://127.0.0.1:$PROXY_PORT" > "$LOG" 2>&1 &
sleep 16

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 "$LOG"; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2
echo "已连上：$(grep -a '界面 History' "$LOG" | tail -1)"

# 点击一律用**窗口相对坐标**（屏幕坐标在 WSLg 上点不动，踩过）
click() { xdotool mousemove --window "$WID" "$1" "$2" click 1; }

echo
echo "===== 4) 进搜索页、打一个字母 ====="
click 185 797            # 底栏：搜索
sleep 3
click 85 626             # 键盘第 2 行第 1 键 'a'
sleep 1

echo
echo "===== 5) 点「搜索」，0.3 秒后立刻点底栏「历史」====="
# 先数一下日志里「正在取观看记录」出现过几次 —— 下面要等它**再**出现一次，
# 否则会把启动时那一条也算进去，得到假的「立即生效」。
N_BEFORE=$(grep -ac '界面 History | 正在取观看记录' "$LOG")
echo "   日志里已有 $N_BEFORE 条（启动时的那次）"

click 392 740            # 「搜索」确认键
sleep 0.3
T0=$(date +%s.%N)
click 62 797             # 底栏：历史
echo "   t=0：刚点完「历史」"

DONE=""
ELAPSED=""
for _ in $(seq 1 400); do
    N_NOW=$(grep -ac '界面 History | 正在取观看记录' "$LOG")
    if [ "$N_NOW" -gt "$N_BEFORE" ]; then
        T1=$(date +%s.%N)
        ELAPSED=$(awk -v a="$T0" -v b="$T1" 'BEGIN{printf "%.2f", b-a}')
        DONE=1
        break
    fi
    sleep 0.05
done

echo
echo "===== 6) 日志时间线 ====="
grep -an '界面 ' "$LOG"

echo
echo "===== 7) 判定 ====="
if [ -n "$DONE" ]; then
    echo "   点「历史」→ 历史页出现，耗时 ${ELAPSED} 秒"
    if awk -v e="$ELAPSED" 'BEGIN{exit !(e < 3.0)}'; then
        echo "✅ **立即生效** —— 搜索还在飞（要 ${SEARCH_DELAY} 秒），界面 1 秒内就切走了"
        echo "   （修复前这个数字会在 ${SEARCH_DELAY} 秒以上：点击排在搜索后面）"
    else
        echo "❌ 耗时 ${ELAPSED}s，还是被排队了"
    fi
else
    echo "❌ 20 秒内没等到历史页 —— 点击被排队"
fi
echo
echo "（附：代理日志，证明搜索确实被拖慢）"
grep -a '拖慢' /tmp/proxy.log | tail -5

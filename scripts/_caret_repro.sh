#!/bin/bash
# 一次性：验证光标不再跑出输入框。
#
# 键盘键位（逻辑，screen 1）：键盘块 y = 1648-108-463 = 1077
#   行1 y=1077+12=1089  键 x = 64 + i*112
#   行2 y=1077+126=1203 键 x = 120 + i*112
#   行3 y=1077+240=1317 键 x = 142 + i*112   ← 只有 7 个字母 z x c v b n m
#   键 100x100，中心 = 起点+50；物理 = 逻辑 x0.5
set -u
export DISPLAY=:0

OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
GW=$(ip route show default | awk '{print $3}')
SERVER="http://$GW:8001"

pkill -x kindle-manga 2>/dev/null && sleep 1
rm -f "$OUT/favorites.json"
export SLINT_SCALE_FACTOR=0.5
nohup "$BIN" --server "$SERVER" > /tmp/caret_app.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/caret_app.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2

click() { xdotool mousemove --window "$WID" "$1" "$2" click 1; sleep 0.2; }
snap()  {
    xdotool mousemove --window "$WID" 5 400
    sleep 1.2
    import -window "$WID" "/tmp/caret_$1.png"
    echo "   截图 caret_$1.png"
}

echo "→ 进搜索页"
click 185 797
sleep 3

# 物理中心：行1 y=570，行2 y=626，行3 y=684；键 x 中心 = (起点+50)*0.5
echo "→ 0 个"; snap 00
echo "→ 打 10 个 (qwertyuiop)"
for i in 0 1 2 3 4 5 6 7 8 9; do click $((57 + i*56)) 570; done
snap 10
echo "→ 打到 19 (+asdfghjkl)"
for i in 0 1 2 3 4 5 6 7 8; do click $((85 + i*56)) 626; done
snap 19
echo "→ 打到 26 (+zxcvbnm)"
for i in 0 1 2 3 4 5 6; do click $((96 + i*56)) 684; done
snap 26

echo
echo "--- 日志 ---"
grep -a '界面 \|候选' /tmp/caret_app.log | tail -3

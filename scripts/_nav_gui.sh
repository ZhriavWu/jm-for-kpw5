#!/bin/bash
# 起桌面版 GUI，停在**搜索页**（键盘竖起 + 底栏都有），给用户看分界线。
#
# 搜索页最能说明问题：键盘的下边就在底栏顶上，一眼能看出
# 「分界线到底画在底栏里、还是侵到键盘那一段」。
set -u
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
GW=$(ip route show default | awk '{print $3}')
SERVER="http://$GW:8001"

printf '%s' "$SERVER" > "$OUT/server.txt"
rm -f "$OUT/favorites.json"

export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5
pkill -x kindle-manga 2>/dev/null && sleep 1

nohup "$BIN" --server "$SERVER" > /tmp/nav_gui.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/nav_gui.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2

# 底栏第 2 格 = 搜索（格宽 1236/5=247.2 逻辑 → 物理 123.6，中心 185）
xdotool mousemove --window "$WID" 185 797 click 1
sleep 3
# 打个字母，让候选栏也出来
xdotool mousemove --window "$WID" 85 626 click 1
sleep 1
xdotool mousemove --window "$WID" 5 300
sleep 1

echo "窗口已就绪，停在搜索页（键盘 + 候选栏 + 底栏都在）"
echo "底栏最上面那条 3px 黑线就是新加回来的分界线。"

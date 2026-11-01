#!/bin/bash
# 一次性：**强制重编** → 起桌面版 → 进收藏页 → 切到「收藏的单话」→ 截图 → 分析。
#
# ⚠️ 两个坑都踩过，别再踩：
#   1. 必须 `touch ui/*.slint` + build，否则跑的是旧二进制（表现为「改了没效果」）。
#   2. 点击必须用 `xdotool mousemove --window $WID X Y click 1`（窗口相对坐标）——
#      屏幕坐标方式在 WSLg 上点不动。
set -u

PROJ=$(cd "$(dirname "$0")/.." && pwd)
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
GW=$(ip route show default | awk '{print $3}')
SERVER="http://$GW:8001"

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga
cd "$PROJ" || exit 1

echo "===== 1) 强制重编 ====="
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^error' /tmp/fb.log | head -10 || true

ALBUM_TITLE='不良女家庭教师 [禁漫汉化组]生意气ギャルの家庭教师、始めます / Learning the Hard Way'
cat > "$OUT/favorites.json" <<EOF
[{"kind":"album","id":"1475681","title":"花丸大榨精 (Meitantei Precure!)","album_id":"","album_title":""},
{"kind":"chapter","id":"238062","title":"第 1 話","album_id":"356488","album_title":"$ALBUM_TITLE"},
{"kind":"chapter","id":"233661","title":"第 1 話","album_id":"356488","album_title":""}]
EOF
printf '%s' "$SERVER" > "$OUT/server.txt"

export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5

pkill -x kindle-manga 2>/dev/null && sleep 1

echo "===== 2) 起 GUI ====="
nohup "$BIN" --server "$SERVER" > /tmp/fav_app.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/fav_app.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2

echo "===== 3) 进「收藏 → 收藏的单话」====="
xdotool mousemove --window "$WID" 433 797 click 1
sleep 4
xdotool mousemove --window "$WID" 285 96 click 1
sleep 5
xdotool mousemove --window "$WID" 5 400
sleep 2

import -window "$WID" /tmp/fav.png
echo "png: $(identify -format '%wx%h' /tmp/fav.png)"
grep -a '界面 Favorites' /tmp/fav_app.log | tail -2

echo
echo "===== 4) 分析 ====="
bash "$PROJ/scripts/_fav_measure.sh"

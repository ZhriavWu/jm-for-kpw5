#!/bin/bash
# 一次性：强制重编 → 起 GUI → **把界面钉在历史页** → 截图 → 量星标。
#
# 为什么要「钉」：第一次跑的时候日志里出现了
#   界面 Search / 界面 History 反复交替
# 说明有东西在点导航栏，截到的可能是搜索页。所以截之前显式点一下「历史」那格。
#
# 坐标（逻辑 → 物理 = x0.5）：
#   底栏 y 1540..1648 → 物理 770..824，中心 y=797
#   5 格，每格宽 1236/5=247.2 → 第 1 格中心 x=123.6 → 物理 62
#   行 i 顶 = 334 + i*133.25；星标框 x=1130、y=行顶+43.625，46x46；星形 42
set -u

PROJ=$(cd "$(dirname "$0")/.." && pwd)
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga
cd "$PROJ" || exit 1

echo "===== 1) 强制重编 ====="
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling 条数=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^error' /tmp/fb.log | head -10 || true

GW=$(ip route show default | awk '{print $3}')
SERVER="http://$GW:8001"

# 历史第 0 条 = 350234（已收藏 → 应实心）
# 历史第 1 条 = 1475681（未收藏 → 应空心）
printf '[{"kind":"album","id":"350234","title":"董卓 上+下","album_id":""}]' > "$OUT/favorites.json"
printf '%s' "$SERVER" > "$OUT/server.txt"

export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5

pkill -x kindle-manga 2>/dev/null && sleep 1

echo "===== 2) 起 GUI ====="
nohup "$BIN" --server "$SERVER" > /tmp/star_app.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/star_app.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 1

echo "===== 3) 钉到历史页（点底栏第 1 格）====="
xdotool mousemove 62 797 click 1
sleep 3
xdotool mousemove 5 5          # 把鼠标挪开，免得悬停干扰
sleep 2

echo "===== 4) 截图 ====="
import -window "$WID" /tmp/star.png
identify -format 'png: %wx%h\n' /tmp/star.png

echo "--- 日志末尾（确认在 History）---"
tail -4 /tmp/star_app.log

echo
echo "===== 5) 量星标（物理 +558/+182 与 +558/+249）====="
python3 "$PROJ/scripts/_star_measure.py" /tmp/star.png \
  "row0_yicangshou:558:182:36:34" \
  "row1_weicangshou:558:249:36:34"

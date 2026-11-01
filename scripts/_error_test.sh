#!/bin/bash
# 一次性：复现「网络错误」这一屏，验证底栏和退出入口都在。
#
# 怎么造出错页：把服务端地址指到一个**没人监听**的端口。
# 候选地址全探不通 + 广播发现也失败 → 进 Screen::Error。
set -u

PROJ=$(cd "$(dirname "$0")/.." && pwd)
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
DEAD=http://127.0.0.1:9      # 9 号端口（discard），本机上没人监听
LOG=/tmp/err_app.log

. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga
cd "$PROJ" || exit 1

echo "===== 1) 强制重编 ====="
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^error' /tmp/fb.log | head -10 || true

# 把候选地址全弄成死的，确保进出错页而不是走运连上
printf '%s' "$DEAD" > "$OUT/server.txt"
rm -f "$OUT/favorites.json"
unset MANGA_SERVER 2>/dev/null || true

export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5
pkill -x kindle-manga 2>/dev/null && sleep 1
: > "$LOG"

echo "===== 2) 起 GUI（指向死地址 $DEAD）====="
env -u MANGA_SERVER "$BIN" --server "$DEAD" > "$LOG" 2>&1 &
sleep 20

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 "$LOG"; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2

echo "--- 日志（应出现「界面 Error」）---"
grep -a '界面 ' "$LOG" | tail -5

echo
echo "===== 3) 截图 + 量底部 ====="
xdotool mousemove --window "$WID" 5 300
sleep 1.5
import -window "$WID" /tmp/err.png
echo "png: $(identify -format '%wx%h' /tmp/err.png)"

echo
echo "===== 4) 逐行看底部（底栏该在物理 y 770..824）====="
python3 - <<'PY'
import subprocess
png = "/tmp/err.png"
r = subprocess.run(["convert", png, "-crop", "618x120+0+704", "+repage",
                    "-colorspace", "Gray", "-depth", "8", "pgm:-"],
                   capture_output=True)
d = r.stdout
st = [0]
def tok():
    p = st[0]
    while d[p:p+1].isspace(): p += 1
    s = p
    while not d[p:p+1].isspace(): p += 1
    st[0] = p
    return d[s:p]
assert tok() == b"P5"
W, H = int(tok()), int(tok()); tok(); st[0] += 1
pix = d[st[0]:st[0]+W*H]
for yy in range(0, H, 2):
    y = 704 + yy
    row = pix[yy*W:(yy+1)*W]
    dark = sum(1 for v in row if v < 128)
    bar = "#" * min(dark // 8, 74)
    mark = ""
    if y in (770, 772): mark = "  ← 底栏顶边 / 分界线"
    if y == 790: mark = "  ← 底栏图标区"
    print("%4d 暗%4d %s%s" % (y, dark, bar, mark))
PY

#!/bin/bash
# 一次性：量底栏顶部有没有那条分界线。
#
# 底栏（逻辑）y 1540..1648 → 物理 770..824。
# 分界线代码在 chrome.slint 里是 NavBar 的 y=0、高 3px（逻辑）→ 物理 770..771。
#
# 判据：看物理 y 768..778 那几行的墨迹。有分界线的话，
#       770/771 两行该是「整行都黑」；被格子盖住的话就是白的。
set -u
export DISPLAY=:0

OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
GW=$(ip route show default | awk '{print $3}')

# ⚠️ **必须先重编** —— 这个脚本原来只管起 GUI，
#    结果量的是上一次编译的二进制，改了跟没改一个样（栽过一次）。
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga
cd "$(dirname "$0")/.." || exit 1
echo "===== 强制重编 ====="
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^error' /tmp/fb.log | head -10 || true
cd "$OUT" || exit 1

printf 'http://%s:8001' "$GW" > "$OUT/server.txt"
rm -f "$OUT/favorites.json"
rm -f /tmp/nav_*.png

pkill -x kindle-manga 2>/dev/null && sleep 1
export SLINT_SCALE_FACTOR=0.5
nohup "$BIN" --server "http://$GW:8001" > /tmp/nav_app.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/nav_app.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2
click() { xdotool mousemove --window "$WID" "$1" "$2" click 1; sleep 2.5; }
snap()  {
    xdotool mousemove --window "$WID" 5 300
    sleep 1.2
    import -window "$WID" "/tmp/nav_$1.png"
}

echo "→ 历史页（无键盘）"; snap history
echo "→ 搜索页（有键盘）"; click 185 797; snap search

echo
echo "===== 底部 60 物理px 的逐行墨迹 ====="
python3 - <<'PY'
import subprocess, os

def rows(png, y0, y1):
    r = subprocess.run(["convert", png, "-crop", "618x%d+0+%d" % (y1-y0, y0),
                        "+repage", "-colorspace", "Gray", "-depth", "8", "pgm:-"],
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
    if tok() != b"P5":
        return None, 0, 0
    W, H = int(tok()), int(tok()); tok()
    st[0] += 1
    pix = d[st[0]:st[0]+W*H]
    return pix, W, H

for tag in ("history", "search"):
    png = "/tmp/nav_%s.png" % tag
    if not os.path.exists(png):
        print("%s: 没有截图" % tag); continue
    pix, W, H = rows(png, 760, 824)
    if pix is None:
        print("%s: 读图失败" % tag); continue
    print("=== %s（物理 y 760..823；底栏顶边 = 物理 770）===" % tag)
    for yy in range(H):
        y = 760 + yy
        row = pix[yy*W:(yy+1)*W]
        dark = sum(1 for v in row if v < 128)
        bar = "#" * min(dark // 8, 70)
        mark = ""
        if y in (770, 771): mark = "  ← 分界线该在这"
        if y == 790: mark = "  ← 图标区开始"
        print("  %3d 暗像素%4d  %s%s" % (y, dark, bar, mark))
    print()
PY

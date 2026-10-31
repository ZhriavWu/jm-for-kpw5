#!/bin/bash
# 光标验证：**先强制重编**，再**一个字符一张图**，最后量每张图里光标的物理宽度。
#
# ⚠️ 必须重编。直接跑二进制会测到旧版本 —— 踩过，表现为「改了没效果」。
#
# 键盘键位（逻辑，screen 1）：键盘块 y = 1648-108-463 = 1077
#   行1 y=1089  键 x 中心 = 64+50+i*112
#   行2 y=1203  键 x 中心 = 120+50+i*112
#   行3 y=1317  键 x 中心 = 142+50+i*112
#   物理 = 逻辑 x0.5
set -u

PROJ=$(cd "$(dirname "$0")" && pwd)
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

rm -f "$OUT/favorites.json"
printf '%s' "$SERVER" > "$OUT/server.txt"

export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5
pkill -x kindle-manga 2>/dev/null && sleep 1

echo "===== 2) 起 GUI ====="
nohup "$BIN" --server "$SERVER" > /tmp/caret_app.log 2>&1 &
sleep 15

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 /tmp/caret_app.log; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 2

click() { xdotool mousemove --window "$WID" "$1" "$2" click 1; sleep 0.15; }
snap()  {
    xdotool mousemove --window "$WID" 5 400
    sleep 0.8
    import -window "$WID" "/tmp/cw_$1.png"
}

echo "===== 3) 进搜索页 ====="
click 185 797
sleep 3

# 一个字符一张图：q w e r t y u i o p a s d f
rm -f /tmp/cw_*.png
snap 00
i=1
for spec in "57 570" "113 570" "169 570" "225 570" "281 570" "337 570" \
            "393 570" "449 570" "505 570" "561 570" \
            "85 626" "141 626" "197 626" "253 626"; do
    set -- $spec
    click "$1" "$2"
    snap "$(printf '%02d' $i)"
    i=$((i+1))
done
echo "   截了 $(ls /tmp/cw_*.png | wc -l) 张"

echo
echo "===== 4) 量每张图里光标的物理宽度 ====="
python3 - <<'PY'
import subprocess, glob, os

def load(png, x0, x1, y0, y1):
    w, h = x1 - x0, y1 - y0
    r = subprocess.run(["convert", png, "-crop", "%dx%d+%d+%d" % (w, h, x0, y0),
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
    return d[st[0]:st[0]+W*H], W, H

# 输入框内部：物理 x 100..576，y 26..62（光标 27 物理px 高，字形竖笔最多 22）
Y0, Y1 = 26, 62
print("%-6s %-10s %-12s %-10s %s" % ("字符数", "光标左缘", "占几列", "总墨量", "宽度"))
prev = None
for f in sorted(glob.glob("/tmp/cw_*.png")):
    n = int(os.path.basename(f)[3:5])
    pix, W, H = load(f, 100, 576, Y0, Y1)
    if pix is None:
        print("%-6d 读图失败" % n); continue
    cols = []
    for xx in range(W):
        ink = sum(1 for yy in range(H) if pix[yy*W+xx] < 128)
        if ink >= 24:
            cols.append((100+xx, ink))
    if not cols:
        print("%-6d 没找到光标" % n); continue
    left = cols[0][0]
    ncols = len(cols)
    ink_sum = sum(c[1] for c in cols)
    # 宽度判定：物理px
    w = ncols
    tag = ""
    if prev is not None and w != prev:
        tag = "  ← 变了！"
    prev = w
    print("%-6d %-10d %-12d %-10d %d px%s" % (n, left, ncols, ink_sum, w, tag))

print()
print("说明：光标逻辑宽 3px，桌面缩放 0.5 → 物理 1.5px。")
print("      渲染器对矩形边做 round() 取整，1.5px 会**交替**落在 1 或 2 物理px 上。")
PY

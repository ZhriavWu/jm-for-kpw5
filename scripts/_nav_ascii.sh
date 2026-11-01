#!/bin/bash
# 把底栏那块画成 ASCII，看清楚到底是什么。
set -u
for tag in history search; do
    png=/tmp/nav_$tag.png
    [ -f "$png" ] || { echo "$png 不存在"; continue; }
    echo "=============================================================="
    echo "=== $tag  $(identify -format '%wx%h' "$png") ==="
    echo "=============================================================="
    python3 - "$png" <<'PY'
import subprocess, sys
png = sys.argv[1]
# 底部 120 物理行，横向 6:1 压缩（`!` 强制两个尺寸，否则高度也被缩）
y0, h = 704, 120
r = subprocess.run(["convert", png, "-crop", "618x%d+0+%d" % (h, y0), "+repage",
                    "-colorspace", "Gray", "-depth", "8",
                    "-resize", "103x%d!" % h, "pgm:-"],
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
assert tok() == b"P5", "不是 P5"
W, H = int(tok()), int(tok()); tok()
st[0] += 1
pix = d[st[0]:st[0]+W*H]
print("（每字符 = 约 6 物理px 宽 × 1 物理px 高；纵轴标的物理 y）")
print("  物理y  逻辑y")
for yy in range(H):
    y = y0 + yy
    line = "".join("#" if pix[yy*W+xx] < 128 else ("+" if pix[yy*W+xx] < 200 else ".") for xx in range(W))
    mark = ""
    if y in (770, 771): mark = "  ← 底栏顶边 / 分界线该在这"
    if y == 700: mark = "  ← 翻页条顶边(逻辑1400)"
    if y == 760: mark = "  ← ?"
    print("%6d %6d  %s%s" % (y, y*2, line, mark))
PY
    echo
done

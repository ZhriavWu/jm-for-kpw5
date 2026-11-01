#!/bin/bash
# 一次性：把「已收藏/未收藏」两行的星标区一起截下来看，判断两态到底分不分得开。
set -u
PROJ=$(cd "$(dirname "$0")/.." && pwd)
OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
GW=$(ip route show default | awk '{print $3}')
SERVER="http://$GW:8001"

export DISPLAY=:0

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
if [ -z "$WID" ]; then
    echo "（没有运行中的实例，起一个）"
    printf '[{"kind":"album","id":"350234","title":"Dong Zhuo","album_id":""}]' > "$OUT/favorites.json"
    printf '%s' "$SERVER" > "$OUT/server.txt"
    export SLINT_SCALE_FACTOR=0.5
    nohup "$BIN" --server "$SERVER" > /tmp/star_app.log 2>&1 &
    sleep 15
    WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
fi

[ -n "$WID" ] || { echo "!! 起不来"; tail -20 /tmp/star_app.log; exit 1; }
echo "wid=$WID"
xdotool windowactivate "$WID" 2>/dev/null
sleep 1
xdotool mousemove 62 797 click 1      # 底栏第 1 格 = 历史
sleep 3
xdotool mousemove 5 5
sleep 1

import -window "$WID" /tmp/star_now.png
identify -format 'png %wx%h\n' /tmp/star_now.png

python3 - /tmp/star_now.png <<'PY'
import subprocess, sys
png = sys.argv[1]

def crop(x, y, w, h, pct=100):
    r = subprocess.run(["convert", png, "-crop", "{0}x{1}+{2}+{3}".format(w, h, x, y),
                        "+repage", "-colorspace", "Gray", "-depth", "8",
                        "-resize", "{0}%".format(pct), "pgm:-"], capture_output=True)
    d = r.stdout
    pos = 0
    def tok():
        nonlocal pos
        while d[pos:pos+1].isspace(): pos += 1
        s = pos
        while not d[pos:pos+1].isspace(): pos += 1
        return d[s:pos]
    assert tok() == b"P5", "不是 P5"
    W, H = int(tok()), int(tok()); tok(); pos += 1
    return W, H, d[pos:pos+W*H]

# 两行星标区一起：物理 x=550..618，y=167..300（行0 y≈189，行1 y≈255）
W, H, pix = crop(548, 165, 70, 136, 50)
print("== 两行一起（原 70x136 -> 缩 50%）==")
print("   上面那颗 = 第0行 = 已收藏（应为实心）")
print("   下面那颗 = 第1行 = 未收藏（应为空心）")
for y in range(H):
    row = pix[y*W:(y+1)*W]
    print("   " + "".join("#" if v < 128 else ("+" if v < 220 else ".") for v in row))

for label, y in (("已收藏", 182), ("未收藏", 249)):
    W, H, pix = crop(556, y, 40, 36, 100)
    ink = sum(1 for v in pix if v < 128)
    print()
    print("== {0}  墨迹 {1}/{2} = {3:.2%} ==".format(label, ink, len(pix), ink/len(pix)))
    for yy in range(H):
        row = pix[yy*W:(yy+1)*W]
        print("   " + "".join("#" if v < 128 else ("+" if v < 220 else ".") for v in row))
PY

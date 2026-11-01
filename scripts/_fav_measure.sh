#!/bin/bash
# 分析 /tmp/fav.png：行的文字带分布 + 封面是否加载。
#
# 收藏页：列表 y 从 258 起（逻辑），行高 142.75；缩略图 72x96 在 x=60，行内居中。
# 物理坐标 = 逻辑 x0.5。
set -u
python3 - <<'PY'
import subprocess
png = "/tmp/fav.png"

def crop_pix(x, y, w, h):
    r = subprocess.run(["convert", png, "-crop", "%dx%d+%d+%d" % (w, h, x, y),
                        "+repage", "-colorspace", "Gray", "-depth", "8", "pgm:-"],
                       capture_output=True)
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

# ---- 1) 行内文字带（只看标题区，避开右侧元信息与星标）----
W, H, pix = crop_pix(80, 129, 360, 143)   # x 160..880 逻辑，两行高
prof = [sum(1 for v in pix[yy*W:(yy+1)*W] if v < 128) for yy in range(H)]
bands, cur = [], None
for i, c in enumerate(prof):
    ly = (129 + i) * 2
    row = 0 if ly < 401 else 1
    rel = ly - (258 if row == 0 else 401)
    if c > 0:
        if cur is None: cur = [row, rel, rel]
        else: cur[2] = rel
    elif cur is not None:
        bands.append(cur); cur = None
if cur: bands.append(cur)
print("=== 文字带（行内相对位置，行高 142.75）===")
for row, a, b in bands:
    print("  行%d: 相对 %3d..%3d  高 %d" % (row, a, b, b - a))
print("  （两行正确的话：行0 应有两条带，约 26..71 和 71..116）")

# ---- 2) 封面 ----
print()
print("=== 缩略图区（逻辑 x60..132，行内居中）===")
for i in (0, 1):
    ly = 258 + 142.75 * i + 71.4 - 48
    W, H, pix = crop_pix(30, int(ly * 0.5), 36, 48)
    nw = sum(1 for v in pix if v < 245)
    print("  行%d: 非白 %.1f%%  %s" % (i, 100 * nw / len(pix),
          "封面已加载" if nw > len(pix) * 0.2 else "空框/没加载"))
PY

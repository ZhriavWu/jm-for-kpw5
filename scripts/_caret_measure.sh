#!/bin/bash
# 分析复现截图：量「末字右缘」到「光标左缘」的距离。
#
# 判定标准：**这个距离应该基本恒定**（就是字形右边的自然留白，几个像素）。
#   * 稳定     → 光标跟着文字走，修好了
#   * 越打越大 → 还在漂（估宽那类误差）
#
# 布局（逻辑）：输入框 x 196..1156；文字区 x 224 起，宽 904
#   光标：3x54 的竖条，紧跟在最后一个字后面
# 物理 = 逻辑 x0.5：文字区 112 起；框右界 578
#
# 光标识别：物理高 27px 的列；字形竖笔一般不超过 16px。
set -u
python3 - <<'PY'
import subprocess

def cols(png, x0, x1, y0, y1):
    r = subprocess.run(["convert", png, "-crop", "%dx%d+%d+%d" % (x1-x0, y1-y0, x0, y0),
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
    pix = d[pos:pos+W*H]
    return [sum(1 for yy in range(H) if pix[yy*W+xx] < 128) for xx in range(W)]

print("%-6s %-10s %-10s %-10s %s" % ("字符", "末字右缘", "光标左缘", "间距(逻辑)", "判定"))
prev = None
for tag in ("00", "10", "19", "26"):
    png = "/tmp/caret_%s.png" % tag
    try:
        hts = cols(png, 100, 578, 28, 62)
    except AssertionError as e:
        print("%-6s 读图失败 %s" % (tag, e)); continue
    tall = [100+i for i, h in enumerate(hts) if h >= 20]
    if not tall:
        print("%-6s 没找到光标列" % int(tag)); continue
    caret = tall[0]
    glyphs = [100+i for i, h in enumerate(hts) if h > 0 and 100+i < caret-1]
    last = max(glyphs) if glyphs else None
    n = int(tag)
    if last is None:
        print("%-6d （还没打字）  光标 物理x=%d" % (n, caret)); continue
    gap = (caret - last) * 2
    if prev is None:
        verdict = ""
    elif abs(gap - prev) <= 4:
        verdict = "✓ 稳定"
    else:
        verdict = "★ 变了 %+d" % (gap - prev)
    prev = gap
    print("%-6d %-10d %-10d %-10d %s" % (n, last, caret, gap, verdict))
print()
print("判定：间距恒定 = 光标跟着文字（修好了）；越打越大 = 还在漂。")
PY

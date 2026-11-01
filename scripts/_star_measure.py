#!/usr/bin/env python3
"""一次性工具：把 PNG 里指定区域裁出来，打印 ASCII 图 + 墨迹占比。

为什么要 ASCII 图：截图看不了（模型不吃图），而「空心/实心」是纯视觉差异，
把像素画成字符才能用眼睛判断。
"""
import subprocess
import sys


def crop_pgm(png, x, y, w, h):
    r = subprocess.run(
        ["convert", png, "-crop", "{0}x{1}+{2}+{3}".format(w, h, x, y),
         "+repage", "-colorspace", "Gray", "-depth", "8", "pgm:-"],
        capture_output=True)
    if r.returncode != 0:
        raise SystemExit("convert 失败: " + r.stderr.decode(errors="replace")[:300])
    data = r.stdout
    pos = 0

    def token():
        nonlocal pos
        while pos < len(data) and data[pos:pos + 1].isspace():
            pos += 1
        if data[pos:pos + 1] == b"#":
            while pos < len(data) and data[pos:pos + 1] != b"\n":
                pos += 1
            return token()
        s = pos
        while pos < len(data) and not data[pos:pos + 1].isspace():
            pos += 1
        return data[s:pos]

    magic = token()
    if magic != b"P5":
        raise SystemExit("不是 P5 PGM: " + repr(magic))
    W = int(token())
    H = int(token())
    token()  # maxval
    pos += 1  # 头尾那一个空白
    return W, H, data[pos:pos + W * H]


def report(png, x, y, w, h, label):
    W, H, pix = crop_pgm(png, x, y, w, h)
    dark = sum(1 for b in pix if b < 128)
    mid = sum(1 for b in pix if 128 <= b < 220)
    total = len(pix) or 1
    print("=== {0}  crop {1}x{2}+{3}+{4} (W={5} H={6}) ===".format(
        label, w, h, x, y, W, H))
    print("    墨迹(<128)      {0:6.2%}  ({1}/{2})".format(dark / total, dark, total))
    print("    浅灰(128-219)   {0:6.2%}".format(mid / total))
    for yy in range(H):
        row = pix[yy * W:(yy + 1) * W]
        line = "".join("#" if b < 128 else ("+" if b < 220 else ".") for b in row)
        print("    " + line)
    print()


if __name__ == "__main__":
    png = sys.argv[1]
    # 参数：label:x:y:w:h
    for spec in sys.argv[2:]:
        label, x, y, w, h = spec.split(":")
        report(png, int(x), int(y), int(w), int(h), label)

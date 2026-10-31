#!/usr/bin/env python3
"""
生成 PW5 尺寸的测试漫画页（JPEG）。

为什么需要：`--demo` 模式走的是 PPM（无编码器、解码路径短），而真机上是
「Python 服务端发 JPEG → 客户端 image crate 解码」。如果不单独用 JPEG 测一遍，
JPEG 解码器有没有正确编进二进制、缩放对不对，都无从验证。

每页画点能一眼看出的东西：
  * 页码（大号、居中）
  * 页宽高比标记（对角线和边框）
  * 逐页变化的灰度条 —— 用来判断翻页是否真的换了页，而不只是换了数字
  * 顶部一条与页码相关的黑条纹，方便肉眼快速分辨

用法：python3 make-test-pages.py <输出目录> [页数]
"""

import os
import sys

from PIL import Image, ImageDraw

W, H = 1236, 1648  # PW5 面板
OUT = sys.argv[1] if len(sys.argv) > 1 else "pages"
N = int(sys.argv[2]) if len(sys.argv) > 2 else 4


def draw_page(idx: int) -> Image.Image:
    im = Image.new("L", (W, H), 255)  # 白底，L 模式即灰度
    d = ImageDraw.Draw(im)

    # 外边框
    d.rectangle([4, 4, W - 5, H - 5], outline=0, width=5)

    # 顶部页码条纹：页数越多条纹越长，一眼能分辨是第几页
    bar_w = int(W * (idx + 1) / (N + 1))
    d.rectangle([4, 4, bar_w, 60], fill=0)

    # 对角线：用来确认缩放没有变形（应该正好从左上到右下）
    d.line([(6, 6), (W - 7, H - 7)], fill=0, width=3)

    # 灰度条：每页不同的灰度，用来判断翻页真的换了内容
    grey = int(255 * (idx + 1) / (N + 1))
    d.rectangle([W // 4, H // 4, W * 3 // 4, H // 4 + 200], fill=grey, outline=0)

    # 大号页码，用内置矢量字体缩放画
    try:
        from PIL import ImageFont

        font = None
        for path in (
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ):
            if os.path.exists(path):
                font = ImageFont.truetype(path, 400)
                break
        text = str(idx)
        if font is not None:
            d.text((W // 2, H // 2), text, fill=0, font=font, anchor="mm")
        else:
            d.text((W // 2, H // 2), text, fill=0, anchor="mm")
    except Exception as e:
        print(f"  字体渲染失败，退回默认：{e}")
        d.text((W // 2, H // 2), str(idx), fill=0, anchor="mm")

    # 底部标注，方便确认尺寸没被改
    d.text((20, H - 60), f"{W}x{H}  page {idx}", fill=0)

    return im


def main():
    os.makedirs(OUT, exist_ok=True)
    for i in range(N):
        im = draw_page(i)
        path = os.path.join(OUT, f"{i}.jpg")
        # 质量 85 与 jm_kindle_server.py 里的设置一致
        im.save(path, "JPEG", quality=85, optimize=True)
        print(f"{path}  {os.path.getsize(path)//1024} KB")
    print(f"共 {N} 页 -> {OUT}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""一次性：用算术复现 render.rs:448 的越界崩溃。

崩溃日志原文：
    range end index 8147716 out of range for slice of length 8147712
8147712 = 1236 × 1648 × 4 —— 正好是画布大小。
"""
VIEW_W, VIEW_H = 1236, 1648
CANVAS_LEN = VIEW_W * VIEW_H * 4

print("画布 = %d × %d × 4 = %d 字节" % (VIEW_W, VIEW_H, CANVAS_LEN))
print("崩溃时的写入位置 = 8147716 = 画布长度 + 4 —— 多写了 1 个像素")
print()

print("哪些图宽会让 dst_h 超过画布高度：")
print("   图宽   scale    step=round(H/scale)  step*scale   dst_h   超出")
bad = []
for w in range(200, 1600, 5):
    scale = VIEW_W / w
    step = round(VIEW_H / scale)
    dh = round(step * scale)
    if dh > VIEW_H:
        bad.append((w, scale, step, step * scale, dh))
for w, scale, step, prod, dh in bad[:12]:
    print("  %5d  %7.3f  %8d          %9.2f  %6d   +%d" % (w, scale, step, prod, dh, dh - VIEW_H))
print("  ...（200..1600 里共 %d 个宽度档会崩）" % len(bad))
print()

w = 800
scale = VIEW_W / w
step = round(VIEW_H / scale)
dh = round(step * scale)
print("精确核对 w=%d 这一档：" % w)
print("  scale = 1236/800          = %.4f" % scale)
print("  step  = round(1648/%.4f) = %d" % (scale, step))
print("  dst_h = round(%d × %.4f) = round(%.3f) = %d" % (step, scale, step * scale, dh))
print("  dy 走到 %d 时 dst_row = %d × %d × 4 = %d" % (VIEW_H, VIEW_H, VIEW_W, VIEW_H * VIEW_W * 4))
print("  di + 4                = %d" % (VIEW_H * VIEW_W * 4 + 4))
print()
print("  崩溃日志报的正是 %d，缓冲区长度 %d —— 完全吻合。" % (VIEW_H * VIEW_W * 4 + 4, CANVAS_LEN))

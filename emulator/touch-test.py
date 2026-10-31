#!/usr/bin/env python3
"""
触摸交互测试 —— 验证「宿主点击 → touchshim → 应用翻页 → 新帧」整条回路。

做的事：
  1. 连上 fb socket 收帧
  2. 连上 touch socket（保持连接，QEMU 的 chardev 是单客户端）
  3. 等第一帧稳定，记下内容指纹
  4. 发一次右侧点击（翻页热区）
  5. 等新帧，比对内容是否变了

判读：
  内容变了  → 触摸回路通，翻页生效
  内容没变  → 触摸没送达，或应用没响应

用法：python3 touch-test.py [每步超时秒数]
"""

import hashlib
import os
import socket
import struct
import sys
import time

FB_SOCK = os.environ.get("FB_STREAM_SOCK", "/tmp/fbstream.sock")
TOUCH_SOCK = os.environ.get("TOUCH_STREAM_SOCK", "/tmp/touchstream.sock")
MAGIC = b"FB8\x00"
HDR = len(MAGIC) + 28
TIMEOUT = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0

PANEL_W, PANEL_H = 1236, 1648


def read_frames(fb, deadline, collect):
    """收到帧就追加 (seq, w, h, sha) 到 collect，返回最后一帧原始数据。"""
    buf = bytearray()
    last = None
    fb.settimeout(1.0)
    while time.time() < deadline:
        try:
            chunk = fb.recv(1 << 16)
            if not chunk:
                break
            buf += chunk
        except socket.timeout:
            continue
        except Exception:
            break
        while True:
            idx = buf.find(MAGIC)
            if idx < 0:
                if len(buf) > 3:
                    del buf[: len(buf) - 3]
                break
            if idx > 0:
                del buf[:idx]
            if len(buf) < HDR:
                break
            w, h, fb_size, flags, _pad, seq = struct.unpack_from("<IIIIIQ", buf, len(MAGIC))
            if fb_size == 0 or fb_size > (1 << 26):
                del buf[:4]
                continue
            if len(buf) < HDR + fb_size:
                break
            payload = bytes(buf[HDR : HDR + fb_size])
            del buf[: HDR + fb_size]
            sha = hashlib.sha1(payload[: w * h]).hexdigest()[:12]
            collect.append((seq, w, h, sha))
            last = (w, h, payload)
    return last


def main():
    print("=== 触摸交互测试 ===")

    # ---- 连 touch（先连，让 QEMU 的 chardev 有客户端）----
    try:
        touch = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        touch.connect(TOUCH_SOCK)
        print(f"  触摸 socket 已连接 {TOUCH_SOCK}")
    except Exception as e:
        print(f"  触摸 socket 连接失败: {e}")
        return 1

    # ---- 连 fb ----
    try:
        fb = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        fb.connect(FB_SOCK)
        print(f"  画面 socket 已连接 {FB_SOCK}")
    except Exception as e:
        print(f"  画面 socket 连接失败: {e}")
        return 1

    # ---- 阶段 1：等首帧 ----
    print(f"\n[1] 等待首帧（最多 {TIMEOUT}s）")
    frames = []
    first = read_frames(fb, time.time() + TIMEOUT, frames)
    if not first:
        print("  没收到任何帧 —— 应用没渲染")
        return 1
    w, h, _ = first
    print(f"  首帧 {w}x{h}，累计 {len(frames)} 帧，指纹 {frames[-1][3]}")
    before = frames[-1][3]

    # ---- 阶段 2：点右侧翻页 ----
    x = int(PANEL_W * 0.85)
    y = int(PANEL_H * 0.50)
    print(f"\n[2] 发触摸：右侧按下释放 ({x},{y})")
    touch.sendall(f"D {x} {y}\n".encode())
    time.sleep(0.15)
    touch.sendall(b"U\n")
    print("  已发送 D/U")

    # ---- 阶段 3：等新帧 ----
    print(f"\n[3] 等待新帧（最多 {TIMEOUT}s）")
    frames.clear()
    read_frames(fb, time.time() + TIMEOUT, frames)
    if not frames:
        print("  点击后没有新帧 —— 触摸没触发重绘")
        return 1
    after = frames[-1][3]
    print(f"  收到 {len(frames)} 帧，指纹 {after}")

    print("\n=== 结论 ===")
    if after != before:
        print(f"  内容已变化：{before} -> {after}")
        print("  翻页成功，触摸回路完全打通 ✓")
        return 0
    print(f"  内容没变（仍是 {before}）")
    print("  触摸没生效，或应用没处理这次点击")
    return 1


if __name__ == "__main__":
    sys.exit(main())

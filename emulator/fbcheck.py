#!/usr/bin/env python3
"""
无头帧检查器 —— 不弹窗口，只确认「应用 → fbshim → socket」这条链路是通的。

用途：在没法看 GUI 的场合（比如远程会话）验证渲染链路。它连上 fb socket，
统计收到多少帧、尺寸多大、像素是否真有内容（不是全白/全黑），并把最后一帧
存成 PNG 便于直接查看。

用法：
    python3 fbcheck.py [秒数]        默认监听 25 秒
"""

import os
import socket
import struct
import sys
import time

FB_SOCK = os.environ.get("FB_STREAM_SOCK", "/tmp/fbstream.sock")
MAGIC = b"FB8\x00"
HDR = len(MAGIC) + 28
DURATION = float(sys.argv[1]) if len(sys.argv) > 1 else 25.0


def main():
    t0 = time.time()
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(5)
        s.connect(FB_SOCK)
        print(f"[fbcheck] 已连接 {FB_SOCK}")
    except Exception as e:
        print(f"[fbcheck] 连接失败: {e}")
        return 1

    buf = bytearray()
    frames = 0
    last = None
    sizes = set()

    s.settimeout(1.0)
    while time.time() - t0 < DURATION:
        try:
            chunk = s.recv(1 << 16)
            if not chunk:
                print("[fbcheck] 对端关闭连接")
                break
            buf += chunk
        except socket.timeout:
            continue
        except Exception as e:
            print(f"[fbcheck] 读取出错: {e}")
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

            frames += 1
            last = (w, h, payload, seq, flags)
            sizes.add((w, h))

    print(f"[fbcheck] 收到 {frames} 帧，耗时 {time.time()-t0:.1f}s")
    if not frames:
        print("[fbcheck] 结论：没有任何帧 —— 应用没渲染，或 shim 没接上")
        return 1

    print(f"[fbcheck] 帧尺寸: {sorted(sizes)}")
    w, h, payload, seq, flags = last
    px = payload[: w * h]
    lo, hi = min(px), max(px)
    dark = sum(1 for b in px if b < 128)
    print(f"[fbcheck] 最后一帧 seq={seq} flags={flags} 像素范围=[{lo},{hi}] 暗像素占比={dark/len(px):.1%}")

    # 存一张 PNG，方便直接看图确认排版
    try:
        from PIL import Image

        img = Image.frombytes("L", (w, h), px)
        out = "/tmp/kindle-frame.png"
        img.save(out)
        print(f"[fbcheck] 最后一帧已存为 {out}")
    except Exception as e:
        print(f"[fbcheck] 存图跳过: {e}")

    if lo == hi:
        print("[fbcheck] 警告：整帧是纯色，画面可能是空白的")
        return 1
    print("[fbcheck] 结论：渲染链路正常，画面有内容")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""
多页翻页与边界测试。

走一遍：前进 3 页 → 再前进（应停在末页）→ 后退 2 页。
每一步都记录画面指纹，用来确认：
  * 每次前进都真的换了内容
  * 翻到末页后再点不会变成空白/崩溃
  * 后退能回得去

用法：python3 pagewalk-test.py [每步等待秒数]
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
STEP_WAIT = float(sys.argv[1]) if len(sys.argv) > 1 else 6.0
PANEL_W, PANEL_H = 1236, 1648


class Fb:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(FB_SOCK)
        self.s.settimeout(0.5)
        self.buf = bytearray()
        self.n = 0

    def _pump(self):
        try:
            chunk = self.s.recv(1 << 16)
            if chunk:
                self.buf += chunk
        except socket.timeout:
            pass
        except Exception:
            pass
        latest = None
        while True:
            idx = self.buf.find(MAGIC)
            if idx < 0:
                if len(self.buf) > 3:
                    del self.buf[: len(self.buf) - 3]
                break
            if idx > 0:
                del self.buf[:idx]
            if len(self.buf) < HDR:
                break
            w, h, fb_size, flags, _p, seq = struct.unpack_from("<IIIIIQ", self.buf, len(MAGIC))
            if fb_size == 0 or fb_size > (1 << 26):
                del self.buf[:4]
                continue
            if len(self.buf) < HDR + fb_size:
                break
            payload = bytes(self.buf[HDR : HDR + fb_size])
            del self.buf[: HDR + fb_size]
            self.n += 1
            latest = hashlib.sha1(payload[: w * h]).hexdigest()[:10]
        return latest

    def settle(self, seconds):
        """收帧直到安静下来，返回最后一次看到的指纹。"""
        end = time.time() + seconds
        last = None
        while time.time() < end:
            r = self._pump()
            if r:
                last = r
            time.sleep(0.05)
        return last

    def wait_first(self, seconds):
        """等到出现第一帧为止（应用启动要 20~30s，不能只等固定几秒）。"""
        end = time.time() + seconds
        while time.time() < end:
            r = self._pump()
            if r:
                return r
            time.sleep(0.05)
        return None


def tap(touch, x, y):
    touch.sendall(f"D {x} {y}\n".encode())
    time.sleep(0.12)
    touch.sendall(b"U\n")


def main():
    fb = Fb()
    touch = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    touch.connect(TOUCH_SOCK)
    print("socket 已连接")

    right = (int(PANEL_W * 0.85), int(PANEL_H * 0.5))
    left = (int(PANEL_W * 0.15), int(PANEL_H * 0.5))

    marks = {}
    # 应用从启动到第一帧要 20~30s（内核启动 + 加载 + 解码），不能只等 STEP_WAIT
    first_timeout = max(60.0, STEP_WAIT * 4)
    print(f"\n[0] 等首帧（最多 {first_timeout:.0f}s）")
    fp = fb.wait_first(first_timeout)
    if not fp:
        print("  没有帧 —— 应用没渲染")
        return 1
    marks["page0"] = fp
    print(f"  page0 = {fp}")
    # 首帧之后可能还有几帧在途，先让它安静下来
    fb.settle(2.0)

    ok = True

    print("\n[1] 连续前进 3 次（点右侧）")
    for i in range(1, 4):
        tap(touch, *right)
        fp = fb.settle(STEP_WAIT)
        key = f"fwd{i}"
        marks[key] = fp
        changed = fp != marks[f"page0" if i == 1 else f"fwd{i-1}"]
        print(f"  前进{i}: {fp}  {'内容已变 ✓' if changed else '内容没变 ✗'}")
        if not changed:
            ok = False

    print("\n[2] 再前进 1 次（第 4 页不存在，应停在末页且不崩）")
    tap(touch, *right)
    fp = fb.settle(STEP_WAIT)
    print(f"  末页再前进: {fp}  {'仍有画面 ✓' if fp else '画面丢失 ✗'}")
    if not fp:
        ok = False
    marks["overflow"] = fp

    print("\n[3] 后退 2 次（点左侧）")
    for i in range(1, 3):
        tap(touch, *left)
        fp = fb.settle(STEP_WAIT)
        key = f"back{i}"
        prev = marks["overflow"] if i == 1 else marks[f"back{i-1}"]
        changed = fp != prev
        print(f"  后退{i}: {fp}  {'内容已变 ✓' if changed else '内容没变 ✗'}")
        if not changed:
            ok = False
        marks[key] = fp

    print("\n=== 汇总 ===")
    for k, v in marks.items():
        print(f"  {k:10s} {v}")
    uniq = len({v for v in marks.values() if v})
    print(f"  不同画面数: {uniq}")
    print("\n" + ("多页翻页与边界行为正常 ✓" if ok else "存在问题 ✗"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

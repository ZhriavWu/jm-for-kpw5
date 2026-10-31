#!/usr/bin/env python3
"""
对着**真实服务端**跑一遍客户端，边走边存截图。

流程：首页 → 最近观看 → 打开第一个本子（章节目录）

每一步都记录画面指纹（sha1），用来证明「确实换了内容」而不是停在原地。
指纹和截图相辅相成：指纹给脚本判断，截图给人看。

用法：python3 _e2e_live.py <输出目录>
"""

import hashlib
import os
import socket
import struct
import sys
import time

FB_SOCK = "/tmp/fbstream.sock"
TOUCH_SOCK = "/tmp/touchstream.sock"
MAGIC = b"FB8\x00"
HDR = len(MAGIC) + 28
PANEL_W, PANEL_H = 1236, 1648

OUT = sys.argv[1] if len(sys.argv) > 1 else "."


class Screen:
    """连 fb socket，随时给出「最新一帧」。"""

    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(FB_SOCK)
        self.s.settimeout(0.3)
        self.buf = bytearray()
        self.count = 0
        self.last = None  # (w, h, payload)

    def pump(self):
        """读干净 socket，留下最后一帧。"""
        got = False
        while True:
            try:
                chunk = self.s.recv(1 << 16)
                if not chunk:
                    break
                self.buf += chunk
            except socket.timeout:
                break
            except Exception:
                break

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
            w, h, size, _flags, _pad, _seq = struct.unpack_from(
                "<IIIIIQ", self.buf, len(MAGIC)
            )
            if size == 0 or size > (1 << 26):
                del self.buf[:4]
                continue
            if len(self.buf) < HDR + size:
                break
            payload = bytes(self.buf[HDR : HDR + size])
            del self.buf[: HDR + size]
            self.count += 1
            self.last = (w, h, payload)
            got = True
        return got

    def wait_frame(self, timeout, settle=2.0):
        """等一帧新的，然后等画面安静下来，返回指纹。"""
        end = time.time() + timeout
        seen = False
        while time.time() < end:
            if self.pump():
                seen = True
            if seen:
                break
            time.sleep(0.05)
        if not seen:
            return None
        # 让它把剩下的帧画完
        end = time.time() + settle
        while time.time() < end:
            self.pump()
            time.sleep(0.05)
        return self.fingerprint()

    def fingerprint(self):
        if not self.last:
            return None
        w, h, payload = self.last
        return hashlib.sha1(payload[: w * h]).hexdigest()[:12]

    def save(self, path):
        if not self.last:
            return False
        w, h, payload = self.last
        try:
            from PIL import Image

            Image.frombytes("L", (w, h), payload[: w * h]).save(path)
            return True
        except Exception as e:
            print(f"  存图失败: {e}")
            return False


class Touch:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(TOUCH_SOCK)

    def tap(self, x, y):
        """按一下。坐标是**面板像素**（0..1235, 0..1647）。"""
        self.s.sendall(f"D {x} {y}\n".encode())
        time.sleep(0.15)
        self.s.sendall(b"U\n")
        print(f"  点击面板 ({x},{y})")


def main():
    print("=== 连上 fb / touch ===")
    scr = Screen()
    touch = Touch()
    print("  已连接")

    marks = {}

    print("\n[1] 等首页（应用启动要 20~30s）")
    fp = scr.wait_frame(90, settle=3.0)
    if not fp:
        print("  没有帧 —— 应用没起来")
        return 1
    marks["1-首页"] = fp
    scr.save(os.path.join(OUT, "01-home.png"))
    print(f"  首页指纹 {fp}")

    # ---- 服务器地址按钮：点开 → 看键盘 → 取消返回 ----
    # 第 4 个按钮「设置服务器地址」在容器 y=628..778，窗口中心 y=767
    print("\n[1b] 点「设置服务器地址」，应弹出键盘并预填当前地址")
    touch.tap(618, 767)
    fp = scr.wait_frame(45, settle=2.5)
    marks["1b-地址键盘"] = fp
    scr.save(os.path.join(OUT, "01b-server-keypad.png"))
    if not fp:
        print("  点完没有新帧 —— 按钮没接上？")
    else:
        print(f"  指纹 {fp}  {'键盘已弹出 ✓' if fp != marks['1-首页'] else '画面没变 ✗'}")

    # 键盘页的「取消」在容器 y=930..1070，窗口中心 y=1064
    print("\n[1c] 点「取消」返回首页")
    touch.tap(618, 1064)
    fp = scr.wait_frame(45, settle=2.5)
    if fp:
        back_ok = fp == marks["1-首页"]
        print(f"  指纹 {fp}  {'已回到首页 ✓' if back_ok else '没回到首页（画面不同）'}")

    # ---- 首页「最近观看」按钮 ----
    # 首页有 5 个按钮，都在容器内（容器 y=64），每行高 150、间距 176：
    #     y=100 搜索漫画      → 窗口 164..314
    #     y=276 输入本子 ID   → 窗口 340..490
    #     y=452 最近观看      → 窗口 516..666，中心 591   ← 就是它
    #     y=628 设置服务器地址 → 窗口 692..842
    #     y=804 重新寻找服务端 → 窗口 868..1018
    # x 都是 80..1156，中心 618。
    # ⚠️ 改动 app.slint 里的按钮布局时，这些数字要跟着改，
    #    否则会点到别的按钮上（加第 5 个按钮时就踩过一次）。
    print("\n[2] 点「最近观看」")
    touch.tap(618, 591)
    fp = scr.wait_frame(45, settle=3.0)
    marks["2-最近观看"] = fp
    scr.save(os.path.join(OUT, "02-history.png"))
    if not fp:
        print("  点完没有新帧")
        return 1
    changed = fp != marks["1-首页"]
    print(f"  指纹 {fp}  {'画面已变 ✓' if changed else '画面没变 ✗'}")
    if not changed:
        return 1

    # ---- 历史列表第一行 ----
    # 历史容器 y=64，第 0 行 y=0..186 → 窗口 y=64..250，中心 157
    print("\n[3] 点第一条观看记录")
    touch.tap(618, 157)
    fp = scr.wait_frame(90, settle=3.5)
    marks["3-本子目录"] = fp
    scr.save(os.path.join(OUT, "03-album.png"))
    if not fp:
        print("  点完没有新帧")
        return 1
    changed = fp != marks["2-最近观看"]
    print(f"  指纹 {fp}  {'画面已变 ✓' if changed else '画面没变 ✗'}")

    # ---- 章节目录第一格（左列第一行）----
    # 目录容器 y=64，格子 y=0..186，左列 x=8..612 → 面板中心约 (310, 157)
    print("\n[4] 打开第一章（进阅读器）")
    touch.tap(310, 157)
    fp = scr.wait_frame(120, settle=4.0)
    marks["4-阅读器"] = fp
    scr.save(os.path.join(OUT, "04-reader.png"))
    if not fp:
        print("  点完没有新帧")
    else:
        changed = fp != marks["3-本子目录"]
        print(f"  指纹 {fp}  {'画面已变 ✓' if changed else '画面没变 ✗'}")

    # ---- 翻页：点右侧 25% 热区 ----
    # 阅读器内容区 y=64..1552，右侧热区 x=927..1236 → 中心约 (1080, 808)
    print("\n[5] 翻到第 2 页（点右侧）")
    touch.tap(1080, 808)
    fp = scr.wait_frame(90, settle=3.5)
    marks["5-第2页"] = fp
    scr.save(os.path.join(OUT, "05-reader-p2.png"))
    if fp:
        print(f"  指纹 {fp}  {'内容已变 ✓' if fp != marks['4-阅读器'] else '内容没变 ✗'}")

    print("\n[6] 再翻到第 3 页")
    touch.tap(1080, 808)
    fp = scr.wait_frame(90, settle=3.5)
    marks["6-第3页"] = fp
    scr.save(os.path.join(OUT, "06-reader-p3.png"))
    if fp:
        print(f"  指纹 {fp}  {'内容已变 ✓' if fp != marks['5-第2页'] else '内容没变 ✗'}")

    print("\n=== 汇总 ===")
    for k, v in marks.items():
        print(f"  {k:14s} {v}")
    print(f"  收到帧总数: {scr.count}")

    print("\n=== 存下的截图 ===")
    for f in ("01-home.png", "02-history.png", "03-album.png",
              "04-reader.png", "05-reader-p2.png", "06-reader-p3.png"):
        p = os.path.join(OUT, f)
        if os.path.exists(p):
            print(f"  {p}  {os.path.getsize(p)} 字节")
    return 0


if __name__ == "__main__":
    sys.exit(main())

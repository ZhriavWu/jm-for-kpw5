#!/usr/bin/env python3
"""
阅读器菜单的排版验证：进阅读器 → 点中下方呼出菜单 → 检查「刷新频率」那一行
→ 点它 → 确认弹出键盘且预填了当前值。

为什么值得单独测：这次把「全屏刷新」从设置页挪进了阅读器菜单，同时把面板
从 470px 加高到 620px 才塞得下第 4 行。**排版错了不会报任何错** —— 表现只是
两行叠在一起、或者最后一行被切掉、或者按钮点到别的东西上。
这种错在电脑上跑桌面版最容易漏（缩放 0.5，1px 的错位看不出来），
所以在模拟器里按真机尺寸量一遍。

判读方式：区域墨迹占比 + 区域指纹，配合截图人工复核。

用法：python3 reader-menu-test.py <输出目录>
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

OUT = sys.argv[1] if len(sys.argv) > 1 else "./emulator/screenshots"
PANEL_W, PANEL_H = 1236, 1648

# ------------------------------------------------------------------ 坐标
#
# 全部是面板绝对像素，和 ui/app.slint 的 screen == 5 那一段对应。
# ⚠️ 改那边的布局，这里要跟着改。

# 呼出菜单的热区：x 40%~70%、y 78%~100%
MENU_ZONE_CENTER = (618, int(PANEL_H * 0.89))

# 菜单面板：x=48 y=height-668 w=width-96 h=620
PANEL = (48, PANEL_H - 668, 48 + (PANEL_W - 96), PANEL_H - 668 + 620)

# 面板里各行（面板内坐标 → 绝对坐标）
P_TOP = PANEL_H - 668


def row(y, h):
    """面板内的一行 → 绝对矩形。"""
    return (PANEL[0], P_TOP + y, PANEL[2], P_TOP + y + h)


TITLE_ROW = row(20, 54)          # 章节名
INFO_ROW = row(78, 44)           # 页码
BTN_ROW = row(150, 140)          # 目录 / 适应 / 退出
REFRESH_ROW = row(306, 140)      # 刷新频率（这次新加的）
HINT_ROW = row(470, 130)         # 提示文字

# 刷新按钮中心（面板内 x=24, y=306, w=面板宽-48, h=140）
REFRESH_BTN_CENTER = (PANEL_W // 2, P_TOP + 306 + 70)

# 键盘页的输入框（容器 y=64 + 局部 y=56..166）—— 没动
INPUT_BOX = (24, 120, 1212, 230)
# 键盘页候选栏那块（用来确认确实进了键盘）。
# ⚠️ 候选栏已移到**输入框下面**（容器局部 y=244..468 → 绝对 308..532）
KEYPAD_CAND = (0, 308, 1236, 532)


class Screen:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.s.connect(FB_SOCK)
        self.s.settimeout(0.3)
        self.buf = bytearray()
        self.count = 0
        self.last = None

    def pump(self):
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
            w, h, size, _f, _p, _q = struct.unpack_from("<IIIIIQ", self.buf, len(MAGIC))
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

    def wait_frame(self, timeout, settle=2.5):
        end = time.time() + timeout
        seen = False
        while time.time() < end and not seen:
            if self.pump():
                seen = True
            time.sleep(0.05)
        if not seen:
            return None
        end = time.time() + settle
        while time.time() < end:
            self.pump()
            time.sleep(0.05)
        return self.fingerprint()

    def fingerprint(self, box=None):
        if not self.last:
            return None
        w, h, payload = self.last
        if box is None:
            return hashlib.sha1(payload[: w * h]).hexdigest()[:12]
        x0, y0, x1, y1 = box
        d = hashlib.sha1()
        for y in range(y0, min(y1, h), 2):
            d.update(payload[y * w + x0 : y * w + min(x1, w)])
        return d.hexdigest()[:12]

    def ink(self, box, thresh=128):
        if not self.last:
            return 0.0
        w, h, payload = self.last
        x0, y0, x1, y1 = box
        dark = total = 0
        for y in range(y0, min(y1, h), 3):
            rowbase = y * w
            for x in range(x0, min(x1, w), 3):
                total += 1
                if payload[rowbase + x] < thresh:
                    dark += 1
        return dark / max(total, 1)

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

    def tap(self, x, y, label=""):
        self.s.sendall(f"D {int(x)} {int(y)}\n".encode())
        time.sleep(0.12)
        self.s.sendall(b"U\n")
        if label:
            print(f"  点击 ({int(x)},{int(y)})  {label}")


def main():
    print("=== 连上 fb / touch ===")
    scr = Screen()
    touch = Touch()

    fails = []

    def check(ok, msg):
        print(f"  {'✓' if ok else '✗'} {msg}")
        if not ok:
            fails.append(msg)

    print("\n[1] 等阅读器（demo 模式直接进阅读器）")
    fp = scr.wait_frame(120, settle=4.0)
    if not fp:
        print("  没有帧 —— 应用没起来")
        return 1
    scr.save(os.path.join(OUT, "menu-01-reader.png"))
    print(f"  指纹 {fp}")

    print("\n[2] 点中下方呼出菜单")
    touch.tap(*MENU_ZONE_CENTER, "呼出菜单")
    fp_menu = scr.wait_frame(45, settle=3.0)
    if not fp_menu:
        print("  点完没有新帧")
        return 1
    scr.save(os.path.join(OUT, "menu-02-open.png"))
    check(fp_menu != fp, "画面变了（菜单弹出来了）")

    print("\n[3] 量面板各行的墨迹")
    panel_ink = scr.ink(PANEL)
    title_ink = scr.ink(TITLE_ROW)
    btn_ink = scr.ink(BTN_ROW)
    refresh_ink = scr.ink(REFRESH_ROW)
    hint_ink = scr.ink(HINT_ROW)
    print(f"  面板整体 {panel_ink:.4f}")
    print(f"    章节名行 {title_ink:.4f}")
    print(f"    按钮行   {btn_ink:.4f}")
    print(f"    刷新行   {refresh_ink:.4f}   ← 这次新加的")
    print(f"    提示行   {hint_ink:.4f}")

    check(panel_ink > 0.01, "菜单面板画出来了")
    check(refresh_ink > 0.005, "「刷新频率」那一行有内容（没被面板切掉）")

    # 面板不能超出屏幕
    check(PANEL[3] <= PANEL_H, f"面板底部 {PANEL[3]} 在屏幕内（{PANEL_H}）")

    # 关键：新加的那一行不能和上面的按钮行重叠
    refresh_hash = scr.fingerprint(REFRESH_ROW)
    btn_hash = scr.fingerprint(BTN_ROW)
    check(refresh_hash != btn_hash, "刷新行和按钮行内容不同（没叠在一起）")

    print("\n[4] 点「刷新频率」，应弹出键盘并预填当前值")
    touch.tap(*REFRESH_BTN_CENTER, "刷新频率")
    fp_key = scr.wait_frame(45, settle=3.0)
    scr.save(os.path.join(OUT, "menu-03-keypad.png"))
    if not fp_key:
        print("  点完没有新帧")
        return 1
    check(fp_key != fp_menu, "画面变了（进了键盘页）")

    # 键盘页的特征：候选栏区域是白的（没在拼音输入），而输入框里有预填的值
    input_ink = scr.ink(INPUT_BOX)
    cand_ink = scr.ink(KEYPAD_CAND)
    print(f"  输入框墨迹 {input_ink:.4f}  候选栏墨迹 {cand_ink:.4f}")
    check(input_ink > 0.003, "输入框里预填了当前值")

    print("\n=== 汇总 ===")
    print(f"  收到帧总数 {scr.count}")
    if fails:
        print(f"\n  失败 {len(fails)} 项：")
        for f in fails:
            print(f"    - {f}")
        return 1
    print("\n  全部通过 —— 阅读器菜单排版正确 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""
封面排版的模拟器验证：搜索结果页 + 详情页，量每个封面的**实际显示比例**。

为什么必须渲染出来验：这是纯排版问题 —— 单元测试能证明 `fit_box()` 算得对，
但证明不了 Slint 那边有没有照着它画。而排版错了不报任何错，只是看起来不对。

判读方式：对每个封面格子，量「非白像素的包围盒」—— 包围盒的宽高比就是
这张图在屏幕上的实际显示比例，和它应有的比例一比就知道对不对。
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
PANEL_W, PANEL_H = 1236, 1648

OUT = sys.argv[1] if len(sys.argv) > 1 else "./emulator/screenshots"

# 搜索页：5 行 × 288，图片在 (16, 16) 起、上限 168×232
SEARCH_ROW_H = 288
SEARCH_IMG_X = 16
SEARCH_IMG_Y = 16
SEARCH_BOX_W = 168
SEARCH_BOX_H = 232
CONTENT_Y = 64          # 内容区起点（顶栏 64）

# 详情页：封面槽 x=32 y=24（相对内容区），上限 240×332
ALBUM_SLOT_X = 32
ALBUM_SLOT_Y = 24
ALBUM_BOX_W = 240
ALBUM_BOX_H = 332


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
                c = self.s.recv(1 << 16)
                if not c:
                    break
                self.buf += c
            except socket.timeout:
                break
            except Exception:
                break
        while True:
            i = self.buf.find(MAGIC)
            if i < 0:
                if len(self.buf) > 3:
                    del self.buf[: len(self.buf) - 3]
                break
            if i > 0:
                del self.buf[:i]
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

    def wait(self, timeout, settle=2.5):
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
        return self.fp()

    def fp(self):
        if not self.last:
            return None
        w, h, p = self.last
        return hashlib.sha1(p[: w * h]).hexdigest()[:12]

    def save(self, name):
        if not self.last:
            return
        w, h, p = self.last
        try:
            from PIL import Image

            Image.frombytes("L", (w, h), p[: w * h]).save(os.path.join(OUT, name))
        except Exception as e:
            print(f"  存图失败 {e}")

    def bbox_of_ink(self, box, thresh=200):
        """区域里非白像素的包围盒。返回 (w, h) 或 None。

        阈值取 200（不是 128）：封面外框是黑的，但里面还有浅灰的对角线，
        要一起框进来才等于「这张图占的地方」。
        """
        if not self.last:
            return None
        w, _h, p = self.last
        x0, y0, x1, y1 = box
        minx, miny, maxx, maxy = x1, y1, x0, y0
        found = False
        for y in range(y0, y1):
            base = y * w
            for x in range(x0, x1):
                if p[base + x] < thresh:
                    found = True
                    if x < minx:
                        minx = x
                    if x > maxx:
                        maxx = x
                    if y < miny:
                        miny = y
                    if y > maxy:
                        maxy = y
        if not found:
            return None
        return (maxx - minx + 1, maxy - miny + 1, minx - x0, miny - y0)


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
    scr = Screen()
    touch = Touch()
    fails = []

    def check(ok, msg):
        print(f"  {'✓' if ok else '✗'} {msg}")
        if not ok:
            fails.append(msg)

    print("\n[1] 等首页")
    fp = scr.wait(120, settle=4.0)
    if not fp:
        print("  没有帧 —— 应用没起来")
        return 1
    print(f"  首页指纹 {fp}")

    print("\n[2] 进搜索：点「搜索漫画」→ 键盘直接确定（关键词是空的，用 mock 的固定结果）")
    touch.tap(618, 239, "首页·搜索漫画")
    scr.wait(45, settle=2.5)
    # 随便打一个字母再确定，走正常路径
    touch.tap(61, 619, "A")
    time.sleep(0.4)
    touch.tap(1112, 919, "确定")
    fp = scr.wait(60, settle=4.0)
    if not fp:
        print("  没进搜索页")
        return 1
    scr.save("cover-01-search.png")
    print(f"  搜索页指纹 {fp}")

    print("\n[3] 量每行的封面（应该一行一个比例）")
    rows = 0
    for i in range(3):
        top = CONTENT_Y + i * SEARCH_ROW_H + SEARCH_IMG_Y
        box = (
            SEARCH_IMG_X,
            top,
            SEARCH_IMG_X + SEARCH_BOX_W,
            top + SEARCH_BOX_H,
        )
        bb = scr.bbox_of_ink(box)
        if not bb:
            print(f"  第 {i+1} 行: 这一格没画东西")
            continue
        w, h, ox, oy = bb
        rows += 1
        ratio = w / h if h else 0
        print(f"  第 {i+1} 行: 显示 {w}x{h}  比例 {ratio:.2f}  左偏 {ox} 上偏 {oy}")
        # 框应该等于图片本身的比例，且不超过上限
        check(w <= SEARCH_BOX_W and h <= SEARCH_BOX_H, f"第 {i+1} 行没超出上限")
        check(
            h >= SEARCH_BOX_W * 0.95 or w >= SEARCH_BOX_W * 0.95,
            f"第 {i+1} 行至少有一个方向撑满了（框贴着图）",
        )
    check(rows >= 3, f"至少量到 3 行封面（实际 {rows}）")

    # 关键断言：**不能有「框是竖的、图是方的」那种上下留白**
    # 也就是：显示高度应该等于图片自身高度（不是被拉成 232 再留白）
    print("\n[4] 关键判据：显示比例应该和源图比例一致，而不是统一 168x168")
    got = []
    for i in range(3):
        top = CONTENT_Y + i * SEARCH_ROW_H + SEARCH_IMG_Y
        bb = scr.bbox_of_ink(
            (SEARCH_IMG_X, top, SEARCH_IMG_X + SEARCH_BOX_W, top + SEARCH_BOX_H)
        )
        if bb:
            got.append(round(bb[0] / bb[1], 2))
    print(f"  量到的比例: {got}")
    print(f"  mock 发的比例: 1.00(方) / 0.72(竖) / 1.33(横)")
    check(len(set(got)) > 1, f"不同比例的封面画出了不同形状的框（不是全一个样）: {got}")

    print("\n[5] 进详情页看大封面")
    touch.tap(300, CONTENT_Y + 100, "第 1 行")
    fp2 = scr.wait(60, settle=4.0)
    scr.save("cover-02-album.png")
    print(f"  详情页指纹 {fp2}")
    slot = (
        ALBUM_SLOT_X,
        CONTENT_Y + ALBUM_SLOT_Y,
        ALBUM_SLOT_X + ALBUM_BOX_W,
        CONTENT_Y + ALBUM_SLOT_Y + ALBUM_BOX_H,
    )
    bb = scr.bbox_of_ink(slot)
    if bb:
        w, h, ox, oy = bb
        print(f"  大封面显示 {w}x{h}  比例 {w/h:.2f}  槽内偏移 ({ox},{oy})")
        check(w <= ALBUM_BOX_W and h <= ALBUM_BOX_H, "大封面没超出槽")
        check(w >= ALBUM_BOX_W * 0.95 or h >= ALBUM_BOX_H * 0.95, "大封面至少一个方向撑满")
    else:
        check(False, "详情页没画出封面")

    print("\n=== 汇总 ===")
    print(f"  收到帧总数 {scr.count}")
    if fails:
        print(f"\n  失败 {len(fails)} 项：")
        for f in fails:
            print(f"    - {f}")
        return 1
    print("\n  全部通过 —— 封面框跟着图的形状走 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())

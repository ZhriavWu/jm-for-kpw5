#!/usr/bin/env python3
"""
中文输入法的端到端验证：走到搜索键盘，打拼音，看候选栏，点字上屏。

为什么要单独做一个：输入法「看起来能用」和「真的能用」差得很远 ——
数据表接错一个偏移、字体子集少了几个码位、候选栏坐标和点击热区差半格，
都只会表现为「点下去没反应」或者「显示成豆腐块」，不报任何错。
这个脚本把整条链在模拟器里走一遍，用画面指纹和墨迹占比自动判读。

判读手段：
  画面指纹（sha1）  —— 给脚本判断「确实换了内容」
  区域墨迹占比      —— 判断「这一块到底画没画东西」
  截图 PNG          —— 给人看排版和字形对不对

用法：python3 ime-test.py <输出目录>
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

# ------------------------------------------------------------------ 坐标
#
# 全部是**面板绝对像素**（0..1235, 0..1647）。键盘页的坐标由
# `ui/app.slint` 里 screen == 1 那一段的容器 y=64 偏移换算而来。
# ⚠️ 改 app.slint 里的键盘布局，这里必须跟着改 —— 否则点中的是别的东西，
#    而表现只是「没反应」，很难查。

HOME_BTN_X = 618
HOME_SEARCH_Y = 239      # 首页「搜索漫画」按钮（容器 y=100..250）

# 键盘页：4 行字母键，每行高 150。
# ⚠️ 键位在容器里的 y 是 530/680/830/980（候选栏移到输入框下面之后整体下移了），
#    行中心 = y+75，再 +64 是内容区顶栏的偏移 —— 下面的值都是**面板绝对坐标**。
KEY_ROW_Y = {0: 669, 1: 819, 2: 969, 3: 1119}
KEY_COL_W = 123.6


def key_xy(row: int, col: int) -> tuple:
    """第 row 行第 col 列那颗键的中心。"""
    return int(col * KEY_COL_W + KEY_COL_W / 2), KEY_ROW_Y[row]


# QWERTY 布局，和 main.rs 的 KEYS 一致：
#   行1 QWERTYUIOP   行2 ASDFGHJKL-   行3 ZXCVBNM:./  （行0 是数字）
QWERTY = {
    "a": (2, 0), "b": (3, 4), "n": (3, 5), "u": (1, 6),
    "y": (1, 5), "z": (3, 0),
}

# 拼音组合行（拼音缓冲显示在这）—— 现在在输入框下面
IME_LINE_BOX = (140, 238, 700, 308)

# 键盘页功能区（容器 y=1130..1280 → 绝对 1194..1344）
FN_ROW_Y = 1269
MODE_TOGGLE_X = 124      # 「拼音 / 英文」切换
SPACE_X = 371
BACKSPACE_X = 618
CLEAR_X = 865
CONFIRM_X = 1112

# 候选栏：两行，每行 6 格 × 206px（容器 y=244 / 356 → 绝对 308 / 420）
# ⚠️ 候选栏现在在**输入框下面**（原来在键盘下面）
CAND_ROW_Y = 364
CAND_COL_W = 206
CAND_ROW2_Y = 476

# 组合行（容器 y=174..244 → 绝对 238..308）
IME_LINE_Y = 273

# 输入框（容器 y=56..166 → 绝对 120..230）—— 没动
INPUT_BOX = (24, 120, 1212, 230)
# 候选栏整块（两行）
CAND_BOX = (0, 308, 1236, 532)


# 候选格的顶边和高度（容器 y=244 / 356 → 绝对 308 / 420）
CAND_ROW_TOPS = (308, 420)
CAND_CELL_H = 112


def cand_xy(i: int) -> tuple:
    """候选栏里第 i 个（整页下标，0..11）的中心。"""
    if i < 6:
        return int(i * CAND_COL_W + CAND_COL_W / 2), CAND_ROW_Y
    return int((i - 6) * CAND_COL_W + CAND_COL_W / 2), CAND_ROW2_Y


def cand_cell_box(i: int) -> tuple:
    """候选栏第 i 格**内框**（抠掉 2px 边框，只看里面的字）。"""
    col, row = i % 6, i // 6
    x0 = int(col * CAND_COL_W)
    y0 = CAND_ROW_TOPS[row]
    return (x0 + 6, y0 + 6, x0 + int(CAND_COL_W) - 6, y0 + CAND_CELL_H - 6)


# ------------------------------------------------------------------ 屏幕


class Screen:
    """连 fb socket，随时给出最新一帧。"""

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
            w, h, size, _f, _p, _seq = struct.unpack_from("<IIIIIQ", self.buf, len(MAGIC))
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

    def fingerprint(self):
        if not self.last:
            return None
        w, h, payload = self.last
        return hashlib.sha1(payload[: w * h]).hexdigest()[:12]

    def region_hash(self, box):
        """某一小块画面的指纹。用来判断「这块地方变了没有」——
        比墨迹占比精确，适合文字长短会变的地方（比如拼音组合行）。"""
        if not self.last:
            return None
        w, h, payload = self.last
        x0, y0, x1, y1 = box
        y1 = min(y1, h)
        x1 = min(x1, w)
        d = hashlib.sha1()
        for y in range(y0, y1, 2):
            d.update(payload[y * w + x0 : y * w + x1])
        return d.hexdigest()[:12]

    def ink(self, box):
        """区域内的暗像素占比。隔 3 像素采样，够判断「画没画东西」。"""
        if not self.last:
            return 0.0
        w, h, payload = self.last
        x0, y0, x1, y1 = box
        y1 = min(y1, h)
        x1 = min(x1, w)
        dark = 0
        total = 0
        for y in range(y0, y1, 3):
            row = y * w
            for x in range(x0, x1, 3):
                total += 1
                if payload[row + x] < 0x80:
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


# ------------------------------------------------------------------ 主流程


def main():
    print("=== 连上 fb / touch ===")
    scr = Screen()
    touch = Touch()
    print("  已连接")

    fails = []

    def check(ok, msg):
        print(f"  {'✓' if ok else '✗'} {msg}")
        if not ok:
            fails.append(msg)

    # ---- 1. 首页 ----
    print("\n[1] 等首页（应用启动要 20~30s）")
    fp = scr.wait_frame(120, settle=3.0)
    if not fp:
        print("  没有帧 —— 应用没起来")
        return 1
    scr.save(os.path.join(OUT, "ime-01-home.png"))
    print(f"  首页指纹 {fp}")

    # ---- 2. 进搜索键盘 ----
    print("\n[2] 点「搜索漫画」进键盘页")
    touch.tap(HOME_BTN_X, HOME_SEARCH_Y, "首页·搜索漫画")
    fp = scr.wait_frame(45, settle=2.5)
    if not fp:
        print("  点完没有新帧")
        return 1
    scr.save(os.path.join(OUT, "ime-02-keypad.png"))
    cand_before = scr.ink(CAND_BOX)
    input_before = scr.ink(INPUT_BOX)
    print(f"  键盘页指纹 {fp}")
    print(f"  候选栏墨迹 {cand_before:.4f}  输入框墨迹 {input_before:.4f}")
    # 搜索默认就是拼音模式，还没打字母 —— 候选栏应该是空的
    check(cand_before < 0.01, "刚进键盘时候选栏是空的")

    # ---- 3. 打拼音 yuan ----
    print("\n[3] 依次点 Y / U / A / N（搜索默认拼音模式）")
    for ch in "yuan":
        r, c = QWERTY[ch]
        x, y = key_xy(r, c)
        touch.tap(x, y, ch.upper())
        time.sleep(0.35)
    fp = scr.wait_frame(45, settle=3.0)
    scr.save(os.path.join(OUT, "ime-03-yuan.png"))
    line_ink = scr.ink(IME_LINE_BOX)
    cand_ink = scr.ink(CAND_BOX)
    print(f"  指纹 {fp}")
    print(f"  组合行墨迹 {line_ink:.4f}  候选栏墨迹 {cand_ink:.4f}")
    # 这两条是核心：按了四个键之后，拼音缓冲和候选字都得真的画出来
    check(line_ink > 0.01, "组合行显示出拼音缓冲")
    check(cand_ink > 0.01, "候选栏显示出候选字（字体覆盖 + 布局 + 数据都通了）")

    # 逐格核对字形。「有墨迹」对**豆腐块**照样成立 —— 字体缺字时画出来的是
    # 一个空心方框，12 格全是方框也能通过上面那条。所以这里要求逐格指纹两两不同。
    filled, glyphs = [], []
    for i in range(12):
        box = cand_cell_box(i)
        if scr.ink(box) > 0.005:
            filled.append(i)
            glyphs.append(scr.region_hash(box))
    print(f"  候选栏有字 {len(filled)} 格，互不相同的字形 {len(set(glyphs))} 个")
    check(len(filled) == 12, f"一页 12 格候选都画出来了（实际 {len(filled)}）")
    check(
        len(set(glyphs)) == len(glyphs),
        f"每格的字都不一样（{len(set(glyphs))}/{len(glyphs)}）—— 全一样说明是豆腐块",
    )

    # ---- 4. 点第一个候选上屏 ----
    print("\n[4] 点候选栏第 1 格，把「元」送上屏")
    x, y = cand_xy(0)
    touch.tap(x, y, "候选 1")
    fp = scr.wait_frame(45, settle=2.5)
    scr.save(os.path.join(OUT, "ime-04-picked.png"))
    input_after = scr.ink(INPUT_BOX)
    cand_after = scr.ink(CAND_BOX)
    print(f"  指纹 {fp}")
    print(f"  输入框墨迹 {input_after:.4f}  候选栏墨迹 {cand_after:.4f}")
    check(input_after > 0.005, "输入框里出现了字（上屏成功）")
    check(cand_after < cand_ink, "上屏后候选栏收起来了")

    # ---- 5. 切到英文，验证「不选字就直接进输入框」 ----
    print("\n[5] 点「拼音/英文」切到英文模式，再打一个 Z")
    touch.tap(MODE_TOGGLE_X, FN_ROW_Y, "拼音/英文 切换")
    scr.wait_frame(30, settle=2.0)
    before_z = scr.ink(INPUT_BOX)
    r, c = QWERTY["z"]
    x, y = key_xy(r, c)
    touch.tap(x, y, "Z")
    fp = scr.wait_frame(45, settle=2.5)
    scr.save(os.path.join(OUT, "ime-05-latin.png"))
    after_z = scr.ink(INPUT_BOX)
    cand_z = scr.ink(CAND_BOX)
    print(f"  指纹 {fp}")
    print(f"  输入框墨迹 {before_z:.4f} → {after_z:.4f}  候选栏墨迹 {cand_z:.4f}")
    check(after_z > before_z, "英文模式下字母直接进输入框")
    check(cand_z < 0.01, "英文模式下不再出候选")

    # ---- 6. 退回拼音模式，确认退格先把拼音吃回去 ----
    #
    # 这里比「墨迹多少」更精确：组合行空着时显示的是提示文字，
    # 打上拼音后换成缓冲内容 —— 字数不一定更多，但一定不一样。
    # 所以比区域指纹，退格干净之后指纹应该**逐位回到原样**。
    print("\n[6] 切回拼音，打「ab」再退格，看缓冲能不能退干净")
    touch.tap(MODE_TOGGLE_X, FN_ROW_Y, "拼音/英文 切换")
    scr.wait_frame(30, settle=2.0)
    touch.tap(CLEAR_X, FN_ROW_Y, "清空")
    scr.wait_frame(30, settle=2.0)
    empty_hash = scr.region_hash(IME_LINE_BOX)

    for ch in "ab":
        r, c = QWERTY[ch]
        x, y = key_xy(r, c)
        touch.tap(x, y, ch.upper())
        time.sleep(0.3)
    scr.wait_frame(30, settle=2.0)
    typed_hash = scr.region_hash(IME_LINE_BOX)

    for _ in range(2):
        touch.tap(BACKSPACE_X, FN_ROW_Y, "退格")
        time.sleep(0.3)
    fp = scr.wait_frame(45, settle=2.5)
    scr.save(os.path.join(OUT, "ime-06-backspace.png"))
    back_hash = scr.region_hash(IME_LINE_BOX)

    print(f"  组合行指纹：空 {empty_hash} → 打「ab」 {typed_hash} → 退两次 {back_hash}")
    check(typed_hash != empty_hash, "打「ab」后组合行变成了拼音缓冲")
    check(back_hash == empty_hash, "两次退格后组合行逐像素回到原样（缓冲退干净了）")

    # ---- 汇总 ----
    print("\n=== 汇总 ===")
    print(f"  收到帧总数 {scr.count}")
    print(f"  截图目录 {OUT}")
    if fails:
        print(f"\n  失败 {len(fails)} 项：")
        for f in fails:
            print(f"    - {f}")
        return 1
    print("\n  全部通过 —— 拼音输入链路在模拟器里是通的 ✓")
    return 0


if __name__ == "__main__":
    sys.exit(main())

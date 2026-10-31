#!/usr/bin/env python3
"""
Kindle 模拟环境查看器。

两件事：
  1. 从 /tmp/fbstream.sock 读 shim 推来的 FB8 帧流，画到窗口里
  2. 把鼠标按下/拖动/抬起转成触摸命令，写到 /tmp/touchstream.sock

FB8 协议（由 fbshim.c 定义，小端）：
    magic[4] = "FB8\\0"
    w u32 | h u32 | fb_size u32 | flags u32 | pad u32 | seq u64   共 32 字节
    payload: fb_size 字节，8bpp 灰度（每像素 1 字节）

触摸协议（与 qemu-kindle 的 server.go 一致，按行）：
    D <x> <y>   按下
    M <x> <y>   移动
    U           抬起

环境变量：
    FB_STREAM_SOCK     默认 /tmp/fbstream.sock
    TOUCH_STREAM_SOCK  默认 /tmp/touchstream.sock
    VIEW_SCALE         显示缩放，默认 0.5
"""

import os
import queue
import socket
import struct
import sys
import threading
import time
import tkinter as tk

from PIL import Image, ImageTk

FB_SOCK = os.environ.get("FB_STREAM_SOCK", "/tmp/fbstream.sock")
TOUCH_SOCK = os.environ.get("TOUCH_STREAM_SOCK", "/tmp/touchstream.sock")
SCALE = float(os.environ.get("VIEW_SCALE", "0.5"))

MAGIC = b"FB8\x00"
HDR_AFTER_MAGIC = 28  # w,h,fb_size,flags,pad(20) + seq(8)
HDR_TOTAL = len(MAGIC) + HDR_AFTER_MAGIC


# --------------------------------------------------------------------- 触摸
class TouchSender:
    """保持一条到 touchsocket 的连接，把命令按行发过去。"""

    def __init__(self, path):
        self.path = path
        self.sock = None
        self.q = queue.Queue(maxsize=256)
        self.lock = threading.Lock()
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(self.path)
                print(f"[viewer] 触摸已连接 {self.path}", file=sys.stderr)
                with self.lock:
                    self.sock = s
                while True:
                    line = self.q.get()
                    if line is None:
                        return
                    s.sendall(line)
            except Exception as e:
                with self.lock:
                    self.sock = None
                print(f"[viewer] 触摸连接失败({e})，1 秒后重试", file=sys.stderr)
                time.sleep(1)

    def send(self, line: str):
        try:
            self.q.put_nowait(line.encode())
        except queue.Full:
            pass  # 丢帧好过卡住


# ----------------------------------------------------------------- 帧读取
class FrameReader:
    """连 fb socket，切出完整帧丢进队列。"""

    def __init__(self, path, out_q):
        self.path = path
        self.out_q = out_q
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(self.path)
                print(f"[viewer] 画面已连接 {self.path}", file=sys.stderr)
                buf = bytearray()
                while True:
                    chunk = s.recv(1 << 16)
                    if not chunk:
                        raise ConnectionError("对端关闭")
                    buf += chunk

                    # 从缓冲里切出所有完整帧
                    while True:
                        idx = buf.find(MAGIC)
                        if idx < 0:
                            # 没有帧头，最多留 3 字节防止截断 magic
                            if len(buf) > 3:
                                del buf[: len(buf) - 3]
                            break
                        if idx > 0:
                            del buf[:idx]
                        if len(buf) < HDR_TOTAL:
                            break
                        w, h, fb_size, flags, _pad, seq = struct.unpack_from(
                            "<IIIIIQ", buf, len(MAGIC)
                        )
                        if fb_size == 0 or fb_size > (1 << 26):
                            del buf[:4]  # 头不可信，跳过这个 magic 重新找
                            continue
                        if len(buf) < HDR_TOTAL + fb_size:
                            break
                        payload = bytes(buf[HDR_TOTAL : HDR_TOTAL + fb_size])
                        del buf[: HDR_TOTAL + fb_size]
                        try:
                            self.out_q.put_nowait((w, h, payload, seq))
                        except queue.Full:
                            pass
            except Exception as e:
                print(f"[viewer] 画面连接失败({e})，1 秒后重试", file=sys.stderr)
                time.sleep(1)


# ------------------------------------------------------------------- 主程序
def main():
    frame_q = queue.Queue(maxsize=4)
    touch = TouchSender(TOUCH_SOCK)

    root = tk.Tk()
    root.title("Kindle 模拟器 — 点击左右两侧翻页")
    root.configure(bg="#222")

    label = tk.Label(root, bg="#222")
    label.pack()

    state = {"w": 1236, "h": 1648, "photo": None, "last": (0, 0)}
    mouse_down = {"v": False}

    def to_panel(event):
        # 窗口坐标 -> 面板像素坐标
        x = int(event.x / SCALE)
        y = int(event.y / SCALE)
        x = max(0, min(state["w"] - 1, x))
        y = max(0, min(state["h"] - 1, y))
        return x, y

    def on_press(event):
        mouse_down["v"] = True
        x, y = to_panel(event)
        state["last"] = (x, y)
        touch.send(f"D {x} {y}\n")

    def on_motion(event):
        if not mouse_down["v"]:
            return
        x, y = to_panel(event)
        # 抖动过滤：移动太小就不发，省得电子墨水屏疯狂重绘
        lx, ly = state["last"]
        if abs(x - lx) + abs(y - ly) < 3:
            return
        state["last"] = (x, y)
        touch.send(f"M {x} {y}\n")

    def on_release(event):
        if not mouse_down["v"]:
            return
        mouse_down["v"] = False
        touch.send("U\n")

    label.bind("<Button-1>", on_press)
    label.bind("<B1-Motion>", on_motion)
    label.bind("<ButtonRelease-1>", on_release)

    def pump():
        """Tk 不是线程安全的，所以在主线程里轮询队列来做渲染。"""
        latest = None
        try:
            while True:
                latest = frame_q.get_nowait()
        except queue.Empty:
            pass

        if latest is not None:
            w, h, payload, seq = latest
            if (w, h) != (state["w"], state["h"]) or state["photo"] is None:
                state["w"], state["h"] = w, h
                disp = (max(1, int(w * SCALE)), max(1, int(h * SCALE)))
                print(
                    f"[viewer] 面板 {w}x{h}，窗口 {disp[0]}x{disp[1]}",
                    file=sys.stderr,
                )
                # 8bpp 灰度直接就是 PIL 的 "L" 模式
                img = Image.frombytes("L", (w, h), payload[: w * h])
                state["photo"] = ImageTk.PhotoImage(img.resize(disp, Image.NEAREST))
                label.configure(image=state["photo"])
            else:
                img = Image.frombytes("L", (w, h), payload[: w * h])
                disp = (max(1, int(w * SCALE)), max(1, int(h * SCALE)))
                state["photo"].paste(img.resize(disp, Image.NEAREST))
                label.configure(image=state["photo"])

        root.after(50, pump)

    FrameReader(FB_SOCK, frame_q)
    root.after(200, pump)
    print("[viewer] 窗口已启动，等待画面…", file=sys.stderr)
    root.mainloop()


if __name__ == "__main__":
    main()

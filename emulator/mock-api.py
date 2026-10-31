#!/usr/bin/env python3
"""
测试用的假服务端（JSON API 版），用来在模拟器里验**封面排版**。

为什么需要：`mock-server.py` 只实现了取图接口，没有 API，所以封面加载不出来 ——
而 cover 的问题是纯排版问题（框和图的形状对不上），单元测试验不了，
只能真渲染出来看。这个脚本补齐 `/api/*` 和 `/cover/*`。

封面是**当场生成的**，而且故意混了几种比例（正方形 / 竖 / 横），
这样一屏之内就能看出「框跟着图走」有没有生效：
    正方形  → 框应该是正方形
    竖图    → 框应该是竖的
    横图    → 框应该是横的
每种封面上画着它自己的尺寸和比例，截图里直接能读出来。

用法：python3 mock-api.py [端口]
"""

import io
import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8001

# 封面用的几种比例：正方形最多（和真实服务端一致 —— 实测 45/48 是方的）
SHAPES = [
    (300, 300),  # 正方形  ← 真实数据里最常见的
    (300, 300),
    (300, 417),  # 竖
    (300, 300),
    (400, 300),  # 横
    (300, 300),
    (300, 300),
    (300, 450),  # 竖（更瘦）
    (300, 300),
    (300, 300),
    (300, 300),
    (300, 210),  # 横
]


def make_cover(album_id: str, w: int, h: int) -> bytes:
    """生成一张封面：白底 + 黑框 + 中间写上尺寸和比例。

    ⚠️ 用 PNG 而不是 JPEG：JPEG 有块状伪影，会让「框对不对」这件事看不清。
    """
    from PIL import Image, ImageDraw

    im = Image.new("L", (w, h), 255)
    d = ImageDraw.Draw(im)
    # 外框（离边 4px），框内留白 —— 这样「图片的边界」在截图上一眼可见
    d.rectangle([4, 4, w - 5, h - 5], outline=0, width=3)
    # 对角线，防止把留白误判成没渲染
    d.line([6, 6, w - 7, h - 7], fill=200, width=1)

    txt = f"{w}x{h}"
    ratio = f"{w/h:.2f}"
    # 居中写尺寸（不依赖字体度量，按字符数估算位置）
    d.text((w // 2 - len(txt) * 3, h // 2 - 14), txt, fill=0)
    d.text((w // 2 - len(ratio) * 3, h // 2 + 6), ratio, fill=0)
    d.text((w // 2 - len(album_id) * 3, h // 2 + 26), album_id, fill=0)

    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


def shape_for(album_id: str):
    """按 id 稳定地挑一种比例（同一个本子每次给同一张图）。"""
    n = sum(ord(c) for c in album_id)
    return SHAPES[n % len(SHAPES)]


def make_page(page: int, w=1236, h=1648) -> bytes:
    from PIL import Image, ImageDraw

    im = Image.new("L", (w, h), 255)
    d = ImageDraw.Draw(im)
    d.rectangle([20, 20, w - 21, h - 21], outline=0, width=6)
    d.text((60, 60), f"page {page}", fill=0)
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


ALBUMS = {
    "100": "测试本子（方形封面）",
    "101": "测试本子（竖封面）",
    "102": "测试本子（横封面）",
}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *a):
        sys.stderr.write(f"[mock-api] {self.address_string()} {fmt % a}\n")

    def _send(self, body: bytes, ctype: str):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, obj):
        self._send(json.dumps(obj).encode(), "application/json")

    def do_GET(self):
        path = self.path.split("?")[0]

        if path == "/" or path == "/api/info":
            self._json(
                {
                    "service": "mock-api",
                    "version": "test",
                    "base_url": f"http://{self.headers.get('Host', '')}",
                    "screen": {"width": 1236, "height": 1648},
                    "search_per_page": 5,
                    "default_sort": "mr",
                }
            )
            return

        if path.startswith("/api/search"):
            self._json(
                {
                    "keyword": "test",
                    "page": 1,
                    "page_count": 1,
                    "total": 3,
                    "items": [
                        {"id": aid, "title": title, "cover": f"/cover/{aid}"}
                        for aid, title in ALBUMS.items()
                    ],
                }
            )
            return

        m = re.match(r"^/api/album/(\w+)$", path)
        if m:
            aid = m.group(1)
            self._json(
                {
                    "id": aid,
                    "title": ALBUMS.get(aid, f"本子 {aid}"),
                    "author": "测试作者",
                    "cover": f"/cover/{aid}",
                    "page_count": 4,
                    "chapter_count": 2,
                    "tags": ["测试", "排版"],
                    "chapters": [
                        {"id": f"{aid}01", "index": "1", "title": "第 1 话", "indextitle": ""},
                        {"id": f"{aid}02", "index": "2", "title": "第 2 话", "indextitle": ""},
                    ],
                }
            )
            return

        m = re.match(r"^/api/photo/(\w+)$", path)
        if m:
            self._json(
                {
                    "id": m.group(1),
                    "page_count": 4,
                    "page_base": 0,
                    "images": [{"url": f"/img/{m.group(1)}/{i}"} for i in range(4)],
                }
            )
            return

        if path.startswith("/api/history"):
            self._json({"items": []})
            return

        m = re.match(r"^/cover/(\w+)$", path)
        if m:
            w, h = shape_for(m.group(1))
            self._send(make_cover(m.group(1), w, h), "image/png")
            return

        m = re.match(r"^/img/([\w]+)/(\d+)$", path)
        if m:
            self._send(make_page(int(m.group(2))), "image/png")
            return

        if path == "/api/tags":
            self._json({"tags": []})
            return

        self.send_error(404, "not found")


def main():
    print(f"[mock-api] 监听 0.0.0.0:{PORT}（guest 通过 10.0.2.2:{PORT} 访问）")
    print(f"[mock-api] 封面比例表: {SHAPES}")
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), H)
    srv.daemon_threads = True
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n[mock-api] 退出")


if __name__ == "__main__":
    main()

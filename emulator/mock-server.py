#!/usr/bin/env python3
"""
测试用的假服务端，模仿 jm_kindle_server.py 的取图接口。

只实现应用真正会用到的那几个路径：
    GET /                        简单的存活页
    GET /img/<photo_id>/<page>   返回该页的 JPEG；超出页数时返回 404

为什么要单独做一个：真机上的数据通路是「服务端发 JPEG → 客户端解码」，
需要验证的不只是解码，还有 ureq 的 HTTP 行为、404 边界处理、跨网络的
实际传输。这个 mock 让整条路可以在模拟器里完整跑通，且不依赖 jmcomic
和外部站点。

用法：python3 mock-server.py <页面目录> [端口]
"""

import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGES_DIR = sys.argv[1] if len(sys.argv) > 1 else "pages"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8000


def page_count() -> int:
    try:
        return len([f for f in os.listdir(PAGES_DIR) if f.endswith(".jpg")])
    except OSError:
        return 0


class Handler(BaseHTTPRequestHandler):
    # 默认的 log_message 会往 stderr 刷屏，改成简洁格式
    def log_message(self, fmt, *args):
        sys.stderr.write(f"[mock] {self.address_string()} {fmt % args}\n")

    def do_GET(self):
        path = self.path.split("?")[0]

        if path == "/":
            body = b"mock server ok\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        if path.startswith("/img/"):
            parts = path.strip("/").split("/")
            # /img/<photo_id>/<page>
            if len(parts) != 3:
                self.send_error(400, "bad image path")
                return
            try:
                page = int(parts[2])
            except ValueError:
                self.send_error(400, "bad page")
                return

            n = page_count()
            if page < 0 or page >= n:
                # 真机服务端在翻到末尾时也是 404，应用据此判断"到头了"
                self.send_error(404, "no such page")
                return

            fp = os.path.join(PAGES_DIR, f"{page}.jpg")
            try:
                with open(fp, "rb") as f:
                    data = f.read()
            except OSError as e:
                self.send_error(500, f"read failed: {e}")
                return

            self.send_response(200)
            self.send_header("Content-Type", "image/jpeg")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return

        self.send_error(404, "not found")


def main():
    n = page_count()
    print(f"[mock] 服务 {n} 页，来自 {os.path.abspath(PAGES_DIR)}")
    print(f"[mock] 监听 0.0.0.0:{PORT}（guest 通过 10.0.2.2:{PORT} 访问）")
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n[mock] 退出")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""一次性：慢代理 —— 把 /api/search 和 /cover/* 拖慢，用来复现「点击排队」。

真机上「加载中」要好几秒（几百 KB 的图 + 解码 + 墨水屏整刷），而这台机器上
服务端 1 毫秒就答了，根本复现不出卡顿。这个代理在中间加延时，
让桌面版表现得像真机，从而能验证「加载期间点击还灵不灵」。

用法：python3 _slow_proxy.py <上游端口> <监听端口> [搜索延时秒] [封面延时秒]
"""
import http.server
import socketserver
import sys
import time
import urllib.request

UPSTREAM_HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1"
UPSTREAM_PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8001
LISTEN = int(sys.argv[3]) if len(sys.argv) > 3 else 8009
SEARCH_DELAY = float(sys.argv[4]) if len(sys.argv) > 4 else 5.0
COVER_DELAY = float(sys.argv[5]) if len(sys.argv) > 5 else 3.0


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, fmt, *a):
        sys.stderr.write("[proxy] " + (fmt % a) + "\n")

    def do_GET(self):
        delay = 0.0
        if self.path.startswith("/api/search"):
            delay = SEARCH_DELAY
        elif self.path.startswith("/cover/"):
            delay = COVER_DELAY

        if delay:
            self.log_message("拖慢 %.1fs: %s", delay, self.path)
            time.sleep(delay)

        url = "http://%s:%d%s" % (UPSTREAM_HOST, UPSTREAM_PORT, self.path)
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                body = r.read()
                self.send_response(r.status)
                for k, v in r.headers.items():
                    if k.lower() in ("content-length", "transfer-encoding", "connection"):
                        continue
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        except Exception as e:
            self.log_message("上游出错: %s", e)
            try:
                self.send_response(502)
                self.send_header("Content-Length", "0")
                self.end_headers()
            except Exception:
                pass


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    print("慢代理: 0.0.0.0:%d -> %s:%d  (search +%.1fs, cover +%.1fs)"
          % (LISTEN, UPSTREAM_HOST, UPSTREAM_PORT, SEARCH_DELAY, COVER_DELAY), flush=True)
    Server(("0.0.0.0", LISTEN), Handler).serve_forever()

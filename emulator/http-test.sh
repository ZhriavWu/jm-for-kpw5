#!/bin/bash
# HTTP 数据通路测试 —— 真机的生产路径。
#
# 验证的是「客户端 ureq 发 HTTP → 服务端返回 JPEG → 客户端解码渲染」，
# 包含 404 边界（翻到末尾）。
#
# 前置：真机上这个服务端是 jm_kindle_server.py；这里用一个只实现
# /img/<photo_id>/<page> 的 mock 顶替，避免依赖 jmcomic 和外部站点。
#
# 网络拓扑（QEMU user/slirp）：
#   guest 10.0.2.15  →  网关/宿主 10.0.2.2
#   mock 服务监听宿主 0.0.0.0:8000，所以 guest 访问 10.0.2.2:8000 即可
#
# 用法：emulator/http-test.sh [超时秒数]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-http.log
MOCKLOG=/tmp/mock-server.log
PORT=8000
WAIT=${1:-30}

rm -f "$FBS" "$TSS" "$LOG" "$MOCKLOG"

# ---- 准备页面 ----
PAGES=/tmp/http-pages
rm -rf "$PAGES"
mkdir -p "$PAGES"
python3 "$PROJ/emulator/make-test-pages.py" "$PAGES" 4 >/dev/null
echo "  测试页已生成：$(ls "$PAGES" | tr '\n' ' ')"

# ---- 起 mock 服务 ----
echo "=== 启动 mock 服务（端口 $PORT）==="
python3 "$PROJ/emulator/mock-server.py" "$PAGES" "$PORT" >"$MOCKLOG" 2>&1 &
MOCK=$!
sleep 1

# 确认宿主侧能拿到图，否则先排除服务端问题
if curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/img/0/0" | grep -q 200; then
  echo "  mock 服务就绪（/img/0/0 -> 200）"
else
  echo "  mock 服务异常，日志："; cat "$MOCKLOG"; kill $MOCK 2>/dev/null; exit 1
fi

cleanup() {
  kill "$MOCK" 2>/dev/null || true
  kill "$QPID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "=== 启动 QEMU（kdev.mode=remote，走 HTTP）==="
qemu-system-arm \
  -M vexpress-a9 -cpu cortex-a9 -m 1024 \
  -kernel "$KDEB/boot/vmlinuz-$KVER" \
  -initrd "$O/initrd.img" \
  -dtb "$KDEB/usr/lib/linux-image-$KVER/vexpress-v2p-ca9.dtb" \
  -drive if=none,id=hd0,file="$O/rootfs-emu.img",format=raw \
  -device virtio-blk-device,drive=hd0 \
  -netdev user,id=net0 \
  -device virtio-net-device,netdev=net0 \
  -chardev socket,id=fbch,path="$FBS",server=on,wait=off \
  -chardev socket,id=touchch,path="$TSS",server=on,wait=off \
  -device virtio-serial-device \
  -device virtserialport,chardev=fbch,name=org.kindle.fb \
  -device virtserialport,chardev=touchch,name=org.kindle.touch \
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=remote kdev.server=http://10.0.2.2:$PORT" \
  -display none -serial "file:$LOG" -no-reboot >/dev/null 2>&1 &
QPID=$!

for i in $(seq 1 60); do
  [ -S "$FBS" ] && [ -S "$TSS" ] && { echo "  socket 就绪"; break; }
  sleep 0.5
done

# 用多页翻页测试跑一遍：它会走 前进3 → 到头 → 后退2
FB_STREAM_SOCK="$FBS" TOUCH_STREAM_SOCK="$TSS" \
  timeout $((WAIT * 8 + 80)) python3 "$PROJ/emulator/pagewalk-test.py" "$WAIT" 2>&1 | tail -30
RC=${PIPESTATUS[0]}

echo
echo "=== guest 侧网络与模式日志 ==="
grep -E '\[guest\].*(模式|eth0|服务端|virtio_net)' "$LOG" | head -12

echo
echo "=== mock 服务收到的请求（证明 HTTP 真的走通了）==="
grep -oE '"GET [^"]*" [0-9]+' "$MOCKLOG" | sort | uniq -c | sort -rn | head -15

echo
echo "=== 应用侧 HTTP 相关错误（若有）==="
grep -iE '请求失败|服务端返回|解码失败|Connection|refused|timed out' "$LOG" | head -8

exit $RC

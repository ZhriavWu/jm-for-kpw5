#!/bin/bash
# QEMU 网络自检 —— 验证 guest 能不能通过 slirp 访问到宿主上的服务。
#
# 纯环境验证：只用 guest 自带的 wget 打宿主，不涉及任何应用代码。
# 通了就说明真机上「客户端 HTTP 取图」这条路的网络底座是好的。
#
# 拓扑：
#   guest 10.0.2.15  ──slirp──  网关/宿主 10.0.2.2
#
# 用法：emulator/net-test.sh [端口]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
LOG=/tmp/emu-net.log
MOCKLOG=/tmp/mock-net.log
PORT=${1:-8000}

rm -f "$LOG" "$MOCKLOG"

PAGES=/tmp/net-pages
rm -rf "$PAGES"; mkdir -p "$PAGES"
python3 "$PROJ/emulator/make-test-pages.py" "$PAGES" 3 >/dev/null

echo "=== 启动 mock 服务（宿主 :$PORT）==="
python3 "$PROJ/emulator/mock-server.py" "$PAGES" "$PORT" >"$MOCKLOG" 2>&1 &
MOCK=$!
sleep 1

if ! curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/" | grep -q 200; then
  echo "  mock 服务在宿主上都不通，先排查它："; cat "$MOCKLOG"
  kill $MOCK 2>/dev/null; exit 1
fi
echo "  宿主侧正常（/ -> 200）"

cleanup() { kill "$MOCK" 2>/dev/null || true; kill "$QPID" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

echo "=== 启动 QEMU（kdev.mode=net）==="
qemu-system-arm \
  -M vexpress-a9 -cpu cortex-a9 -m 1024 \
  -kernel "$KDEB/boot/vmlinuz-$KVER" \
  -initrd "$O/initrd.img" \
  -dtb "$KDEB/usr/lib/linux-image-$KVER/vexpress-v2p-ca9.dtb" \
  -drive if=none,id=hd0,file="$O/rootfs-emu.img",format=raw \
  -device virtio-blk-device,drive=hd0 \
  -netdev user,id=net0 \
  -device virtio-net-device,netdev=net0 \
  -device virtio-serial-device \
  -chardev socket,id=fbch,path=/tmp/fbstream.sock,server=on,wait=off \
  -chardev socket,id=touchch,path=/tmp/touchstream.sock,server=on,wait=off \
  -device virtserialport,chardev=fbch,name=org.kindle.fb \
  -device virtserialport,chardev=touchch,name=org.kindle.touch \
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=net kdev.server=http://10.0.2.2:$PORT" \
  -display none -serial "file:$LOG" -no-reboot >/dev/null 2>&1 &
QPID=$!

echo "  等待自检完成（最多 90s）…"
for i in $(seq 1 90); do
  grep -q '自检结束' "$LOG" 2>/dev/null && break
  sleep 1
done

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null
sleep 1

echo
echo "========== guest 侧自检输出 =========="
sed -n '/配置网络/,$p' "$LOG" | head -40

echo
echo "========== mock 服务收到的请求 =========="
grep -oE '"GET [^"]*" [0-9]+' "$MOCKLOG" | sort | uniq -c | sort -rn | head -10

echo
echo "========== 结论 =========="
if grep -qE '文件头 *ffd8' "$LOG" 2>/dev/null; then
  echo "  网络通了，而且真的从宿主取到了一张 JPEG ✓"
  RC=0
elif grep -q '服务端可达' "$LOG" 2>/dev/null; then
  echo "  网络通了（HTTP 有响应），但取图那步有问题，看上面输出"
  RC=1
else
  echo "  网络不通 —— 看上面 guest 输出定位"
  RC=1
fi
exit $RC

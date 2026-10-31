#!/bin/bash
# 启动 Kindle 模拟环境：QEMU 跑 Kindle 系统 + 窗口显示与鼠标触摸
#
# 用法：
#   emulator/run-emulator.sh          启动（窗口显示，鼠标点按）
#   emulator/run-emulator.sh --shell  不跑应用，直接进 Kindle 里的 shell 调试
#   emulator/run-emulator.sh --logs   跟随串口日志
#
# 想连真服务端就加 --server，地址必须是**能从 WSL 访问到**的那个：
#   emulator/run-emulator.sh --server http://172.19.48.1:8001
#
# 注意：不能写 10.0.2.2。那是 QEMU slirp 里的「网关」，指的是跑 QEMU 的这台机器，
# 也就是 WSL 自己 —— 而服务端在 Windows 上。Windows 在 WSL 眼里的地址是默认网关
# （`ip route show default` 的第三列，vEthernet 那块的 172.x）。
#
# 画面链路：
#   应用写 /dev/fb0 → fbshim 拦截并推 FB8 帧 → virtio-serial → 宿主 viewer.py 窗口
#   鼠标点击 → viewer.py 发 "D x y" → virtio-serial → touchshim 伪造 evdev 事件 → 应用

set -euo pipefail

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
EMU=$ROOT/emulator
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}

FB_SOCK=/tmp/fbstream.sock
TOUCH_SOCK=/tmp/touchstream.sock
BOOTLOG=/tmp/kindle-emu-boot.log

MODE=""
SERVER=""
while [ $# -gt 0 ]; do
  case "$1" in
    --shell|--logs) MODE="$1"; shift ;;
    --server)       SERVER="${2:-}"; shift 2 ;;
    *)              shift ;;
  esac
done

# 上一次跑剩的 socket 会让 QEMU 起不来
[ -e "$FB_SOCK" ] && rm -f "$FB_SOCK"
[ -e "$TOUCH_SOCK" ] && rm -f "$TOUCH_SOCK"

[ -f "$O/initrd.img" ] || { echo "缺少 $O/initrd.img，先跑 emulator/build-emulator.sh"; exit 1; }
[ -f "$O/rootfs-emu.img" ] || { echo "缺少 $O/rootfs-emu.img，先跑 emulator/build-emulator.sh"; exit 1; }

APPEND="root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4"
if [ "$MODE" = "--shell" ]; then
  APPEND="$APPEND kdev.norun=1"
  echo "== shell 模式：不自动启动应用 =="
fi
if [ -n "$SERVER" ]; then
  # kdev.mode=remote 让 guest 走 HTTP 取图（真机的生产路径），
  # 而不是默认的 demo（本地生成测试图）
  APPEND="$APPEND kdev.mode=remote kdev.server=$SERVER"
  echo "== 连接服务端：$SERVER =="
fi

echo "== 启动 QEMU（Kindle 系统）=="
qemu-system-arm \
  -M vexpress-a9 -cpu cortex-a9 -m 1024 \
  -kernel "$KDEB/boot/vmlinuz-$KVER" \
  -initrd "$O/initrd.img" \
  -dtb "$KDEB/usr/lib/linux-image-$KVER/vexpress-v2p-ca9.dtb" \
  -drive if=none,id=hd0,file="$O/rootfs-emu.img",format=raw \
  -device virtio-blk-device,drive=hd0 \
  -netdev user,id=net0 \
  -device virtio-net-device,netdev=net0 \
  -chardev socket,id=fbch,path="$FB_SOCK",server=on,wait=off \
  -chardev socket,id=touchch,path="$TOUCH_SOCK",server=on,wait=off \
  -device virtio-serial-device \
  -device virtserialport,chardev=fbch,name=org.kindle.fb \
  -device virtserialport,chardev=touchch,name=org.kindle.touch \
  -append "$APPEND" \
  -display none -serial "file:$BOOTLOG" -no-reboot \
  2>/dev/null &
QEMU_PID=$!

cleanup() {
  kill "$QEMU_PID" 2>/dev/null || true
  wait "$QEMU_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "  QEMU pid=$QEMU_PID，串口日志：$BOOTLOG"

if [ "$MODE" = "--logs" ]; then
  sleep 3
  tail -f "$BOOTLOG"
  exit 0
fi

# 等 QEMU 把 socket 建出来
for _ in $(seq 1 40); do
  [ -S "$FB_SOCK" ] && break
  sleep 0.25
done

if [ -S "$FB_SOCK" ]; then
  echo "== 启动查看器窗口（鼠标点击 = 触摸，左/右侧 = 翻页）=="
  FB_STREAM_SOCK="$FB_SOCK" TOUCH_STREAM_SOCK="$TOUCH_SOCK" \
    python3 "$PROJ/emulator/viewer.py"
else
  echo "!! 没等到 $FB_SOCK，QEMU 可能启动失败。日志："
  tail -20 "$BOOTLOG"
  exit 1
fi

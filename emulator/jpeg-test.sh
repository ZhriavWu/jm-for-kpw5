#!/bin/bash
# JPEG 路径测试：用 --dir 模式跑，验证「真机那条 JPEG 解码 + 多页翻页」的路。
#
# 为什么单独测：--demo 走 PPM，绕过了 JPEG 解码器。真机上是服务端发 JPEG、
# 客户端 image crate 解码，所以必须单独验证，否则解码器没编进去都发现不了。
#
# 用法：emulator/jpeg-test.sh [超时秒数]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-jpeg.log
WAIT=${1:-25}

rm -f "$FBS" "$TSS" "$LOG"

echo "=== 启动 QEMU（kdev.mode=dir，走 JPEG）==="
qemu-system-arm \
  -M vexpress-a9 -cpu cortex-a9 -m 1024 \
  -kernel "$KDEB/boot/vmlinuz-$KVER" \
  -initrd "$O/initrd.img" \
  -dtb "$KDEB/usr/lib/linux-image-$KVER/vexpress-v2p-ca9.dtb" \
  -drive if=none,id=hd0,file="$O/rootfs-emu.img",format=raw \
  -device virtio-blk-device,drive=hd0 \
  -chardev socket,id=fbch,path="$FBS",server=on,wait=off \
  -chardev socket,id=touchch,path="$TSS",server=on,wait=off \
  -device virtio-serial-device \
  -device virtserialport,chardev=fbch,name=org.kindle.fb \
  -device virtserialport,chardev=touchch,name=org.kindle.touch \
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=dir" \
  -display none -serial "file:$LOG" -no-reboot >/dev/null 2>&1 &
QPID=$!
echo "  QEMU pid=$QPID"

for i in $(seq 1 60); do
  [ -S "$FBS" ] && [ -S "$TSS" ] && { echo "  socket 就绪"; break; }
  sleep 0.5
done

FB_STREAM_SOCK="$FBS" TOUCH_STREAM_SOCK="$TSS" \
  timeout $((WAIT * 2 + 40)) python3 "$PROJ/emulator/touch-test.py" "$WAIT"
RC=$?

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null
sleep 1

echo
echo "=== guest 日志 ==="
grep -E '\[guest\]|touchshim\] (down|up)|平台后端' "$LOG" | head -25

echo
echo "=== 解码相关错误（若有）==="
grep -iE '解码失败|decode|jpeg|error' "$LOG" | grep -viE 'GIC CPU|device link' | head -10

exit $RC

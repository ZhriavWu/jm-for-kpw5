#!/bin/bash
# 多页翻页 + 边界测试：在 --dir 模式（4 页 JPEG）下走一遍前进/到头/后退。
#
# 用法：emulator/pagewalk-test.sh [每步等待秒数]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-pagewalk.log
STEP=${1:-6}

rm -f "$FBS" "$TSS" "$LOG"

echo "=== 启动 QEMU（4 页 JPEG，kdev.mode=dir）==="
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

for i in $(seq 1 60); do
  [ -S "$FBS" ] && [ -S "$TSS" ] && { echo "  socket 就绪"; break; }
  sleep 0.5
done

FB_STREAM_SOCK="$FBS" TOUCH_STREAM_SOCK="$TSS" \
  timeout $((STEP * 8 + 60)) python3 "$PROJ/emulator/pagewalk-test.py" "$STEP"
RC=$?

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null

echo
echo "=== 触摸事件序列（应用实际收到的）==="
grep -oE 'touchshim\] (down [0-9]+,[0-9]+|up)' "$LOG" | head -20

exit $RC

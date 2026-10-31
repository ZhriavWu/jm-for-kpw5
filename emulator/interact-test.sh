#!/bin/bash
# 触摸交互测试：起 QEMU，等渲染稳定，然后点右侧翻页，验证画面是否变化。
#
# 用法：emulator/interact-test.sh [超时秒数]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-interact.log
WAIT=${1:-30}

rm -f "$FBS" "$TSS" "$LOG"

echo "=== 启动 QEMU ==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4" \
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
echo "=== touchshim 日志（看重连是否已消除）==="
grep -c 'vport connected' "$LOG" | sed 's/^/  vport connected 次数: /'
grep 'touchshim' "$LOG" | tail -12

exit $RC

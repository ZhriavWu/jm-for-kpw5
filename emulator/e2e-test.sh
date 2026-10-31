#!/bin/bash
# 端到端冒烟测试：不弹窗口，验证「应用 → fbshim → socket」渲染链路是通的。
#
# 用法：emulator/e2e-test.sh [监听秒数] [模式]
#   模式：demo（默认，本地生成测试图）| dir（读 JPEG）| kual（走 launch.sh）
#
# 判读标准：
#   fbcheck 报「收到 N 帧」且「画面有内容」 → 链路 OK
#   报「没有任何帧」                        → 看下面的串口日志，多半是应用起不来

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-e2e.log
WAIT=${1:-45}
MODE=${2:-demo}

rm -f "$FBS" "$TSS" "$LOG"

echo "=== 启动 QEMU（后台）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=$MODE" \
  -display none -serial "file:$LOG" -no-reboot >/dev/null 2>&1 &
QPID=$!
echo "  QEMU pid=$QPID  模式=$MODE"

for i in $(seq 1 60); do
  [ -S "$FBS" ] && { echo "  fb socket 就绪"; break; }
  sleep 0.5
done

echo "=== 帧检查器（最多 ${WAIT}s）==="
FB_STREAM_SOCK="$FBS" timeout $((WAIT + 8)) python3 "$PROJ/emulator/fbcheck.py" "$WAIT"
RC=$?

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null
sleep 1

echo
echo "=== guest 侧日志 ==="
sed -n '/\[guest\]/,$p' "$LOG" | head -50

echo
echo "=== 内核/shim 错误行 ==="
grep -iE 'error|fail|cannot|denied|segfault|panic|not found' "$LOG" | grep -v 'Failed to create device link' | head -20

exit $RC

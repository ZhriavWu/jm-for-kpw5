#!/bin/bash
# 在模拟器里验证 KUAL 的 launch.sh —— 真机上真正被 KUAL 调用的就是它，
# 一旦有 bug，用户看到的就是一块黑屏。
#
# 为什么这样能测：模拟器的 rootfs 就是真实的 Kindle 文件系统，lipc-set-prop、
# eips、killall、usleep 这些 Kindle 专有命令都在。LD_PRELOAD 会沿进程树继承，
# 所以从 guest-init.sh 里设好后，launch.sh 及其子进程都能拿到 shim。
#
# 测的是什么：launch.sh 的脚本逻辑 —— 路径解析、日志落盘、暂停/恢复流程、
# 退出后是否正常交还屏幕。**不是**测真机那份静态二进制。
#
# 用法：emulator/kual-test.sh [超时秒数]

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-kual.log
WAIT=${1:-30}

rm -f "$FBS" "$TSS" "$LOG"

echo "=== 启动 QEMU（kdev.mode=kual，走 launch.sh）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=kual" \
  -display none -serial "file:$LOG" -no-reboot >/dev/null 2>&1 &
QPID=$!

for i in $(seq 1 60); do
  [ -S "$FBS" ] && [ -S "$TSS" ] && { echo "  socket 就绪"; break; }
  sleep 0.5
done

echo "=== 等首帧（launch.sh 里多了暂停框架的步骤，会更慢）==="
FB_STREAM_SOCK="$FBS" TOUCH_STREAM_SOCK="$TSS" \
  timeout $((WAIT * 2 + 40)) python3 "$PROJ/emulator/touch-test.py" "$WAIT"
RC=$?

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null
sleep 1

echo
echo "=== launch.sh 的完整输出（guest 侧）==="
sed -n '/模式 kual/,$p' "$LOG" | head -40

echo
echo "=== launch.sh 是否报错 ==="
grep -iE 'not found|No such|Permission denied|cannot|找不到' "$LOG" | grep -v 'GIC CPU' | head -10

echo
echo "=== 日志文件是否落到 /mnt/us/kindle-manga-logs/ ==="
grep -A3 '它写下的日志' "$LOG" | head -8

exit $RC

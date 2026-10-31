#!/bin/bash
# 阅读器菜单的模拟器验证：进阅读器 → 呼出菜单 → 量「刷新频率」那一行的排版。
#
# 用 demo 模式（本地生成测试页），不走网络 —— 应用会直接落在阅读器里，
# 正是我们要验的那个界面。
#
# 用法：emulator/reader-menu-test.sh
# 产出：emulator/screenshots/menu-*.png

set -uo pipefail

export KDEV=${KDEV:-/opt/kindle-dev}
export PATH="$KDEV/bin:$PATH"
export PATH="$HOME/x-tools/arm-kindlehf-linux-gnueabihf/bin:$PATH"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/target/kindle-manga}

PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
ROOT=${ROOT:-/opt/kindle-dev}
KVER=${KVER:-6.12.94+deb13-armmp}
O=$ROOT/emulator/out
KDEB=$ROOT/qemu/armmp
FBS=/tmp/fbstream.sock
TSS=/tmp/touchstream.sock
LOG=/tmp/emu-menu.log
SHOT=$PROJ/emulator/screenshots

# LOGLEVEL=debug 会放开客户端日志（走 kdev.log= 传给 guest）。
# 用来确认「阅读器里到底有没有触发全屏刷新」—— fbshim 不拦 MXCFB ioctl，
# 模拟器里没有别的观察手段。
LOGLEVEL=${LOGLEVEL:-}
LOGOPT=""
[ -n "$LOGLEVEL" ] && LOGOPT="kdev.log=$LOGLEVEL"

mkdir -p "$SHOT"
pkill -x qemu-system-arm 2>/dev/null
sleep 1
rm -f "$FBS" "$TSS" "$LOG"

echo "=== 构建 ==="
if ! bash "$PROJ/emulator/build-emulator.sh" >/tmp/menu-build.log 2>&1; then
    echo "  构建失败，见 /tmp/menu-build.log"
    tail -25 /tmp/menu-build.log
    exit 1
fi
echo "  OK"

echo
echo "=== 启动 QEMU（demo 模式，直接进阅读器）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=demo $LOGOPT" \
  -display none -serial "file:$LOG" -no-reboot >/tmp/qemu-menu.err 2>&1 &
QPID=$!

cleanup() { kill "$QPID" 2>/dev/null; wait "$QPID" 2>/dev/null; }
trap cleanup EXIT INT TERM

for _i in $(seq 1 60); do
    [ -S "$FBS" ] && [ -S "$TSS" ] && break
    kill -0 "$QPID" 2>/dev/null || { echo "QEMU 启动失败："; cat /tmp/qemu-menu.err; exit 1; }
    sleep 0.5
done

echo
echo "=== 驱动 UI ==="
timeout 600 python3 "$PROJ/emulator/reader-menu-test.py" "$SHOT"
RC=$?

cleanup
trap - EXIT INT TERM

# 刷新策略：阅读器里**应该**有全刷，这是和首页/设置的关键区别。
# 需要 LOGLEVEL=debug 才会出现那行 debug 日志。
echo
echo "=== 阅读器内的全屏刷新 ==="
if [ -n "$LOGLEVEL" ]; then
    N=$(grep -ac '按策略清残影' "$LOG" 2>/dev/null || echo 0)
    echo "  阅读器里按策略触发的全屏刷新: $N 次"
    if [ "$N" -gt 0 ]; then
        echo "  ✓ 阅读器里全刷正常工作"
    else
        echo "  ✗ 阅读器里一次全刷都没有 —— 门控开反了？"
    fi
    grep -a '离开阅读器' "$LOG" | head -3
else
    echo "  （加 LOGLEVEL=debug 跑才能看到）"
fi

echo
echo "=== 截图 ==="
ls -la "$SHOT"/menu-*.png 2>/dev/null || echo "  （没有截图）"

exit $RC

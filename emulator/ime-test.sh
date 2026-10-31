#!/bin/bash
# 中文输入法的模拟器验证：构建 → 起 QEMU → 走到搜索键盘 → 打拼音 → 截图判读。
#
# 和 live-test.sh 的区别：那个要一个真在跑的服务端，这个**不需要** ——
# 输入法只跟键盘和候选栏有关，服务端连不上也照样能验。
# 所以这里故意把 MANGA_SERVER 指到一个没人监听的端口，让它快速失败、
# 停在首页（首页那几个按钮就是入口）。
#
# 用法：emulator/ime-test.sh
# 产出：emulator/screenshots/ime-*.png

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
LOG=/tmp/emu-ime.log
SHOT=$PROJ/emulator/screenshots

# 没人监听的端口 —— 应用探活会立刻被拒，然后停在首页报错。
SRV=http://10.0.2.2:9

# LOGLEVEL=debug 会把客户端的日志放开到 debug（走 kdev.log= 传给 guest）。
# 用来看「每次刷新到底走的全刷还是局部刷」—— 这条路径在模拟器里没有别的
# 观察手段：fbshim 只截帧缓冲的写，不碰 MXCFB 的 ioctl。
LOGLEVEL=${LOGLEVEL:-}
LOGOPT=""
[ -n "$LOGLEVEL" ] && LOGOPT="kdev.log=$LOGLEVEL"

mkdir -p "$SHOT"

pkill -x qemu-system-arm 2>/dev/null
sleep 1
rm -f "$FBS" "$TSS" "$LOG"

echo "=== 构建（含新的输入法代码）==="
if ! bash "$PROJ/emulator/build-emulator.sh" >/tmp/ime-build.log 2>&1; then
    echo "  构建失败，见 /tmp/ime-build.log"
    tail -25 /tmp/ime-build.log
    exit 1
fi
echo "  OK"

echo
echo "=== 启动 QEMU（kdev.mode=remote，服务端故意不可达）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=remote kdev.server=$SRV $LOGOPT" \
  -display none -serial "file:$LOG" -no-reboot >/tmp/qemu-ime.err 2>&1 &
QPID=$!

cleanup() { kill "$QPID" 2>/dev/null; wait "$QPID" 2>/dev/null; }
trap cleanup EXIT INT TERM

for _i in $(seq 1 60); do
    [ -S "$FBS" ] && [ -S "$TSS" ] && break
    kill -0 "$QPID" 2>/dev/null || { echo "QEMU 启动失败："; cat /tmp/qemu-ime.err; exit 1; }
    sleep 0.5
done

echo
echo "=== 驱动 UI ==="
timeout 600 python3 "$PROJ/emulator/ime-test.py" "$SHOT"
RC=$?

cleanup
trap - EXIT INT TERM

echo
echo "=== 客户端日志里的界面轨迹 ==="
grep -E '界面|拼音|键盘模式' "$LOG" | head -20

# 刷新策略的证据。
#
# 为什么只能靠日志：fbshim 只截帧缓冲的写（mmap / write），不碰 MXCFB 的
# SEND_UPDATE ioctl —— 所以「这一帧走的是全刷还是局部刷」在模拟器里没有
# 别的观察手段。策略决定走全刷时会打一行 debug，数它就行。
#
# 需要 LOGLEVEL=debug 才会出现。默认间隔是每 1 次刷新全刷，
# 所以正常情况下这个数字应该和刷新次数同量级。
echo
echo "=== 刷新策略 ==="
grep -E '全屏刷新：' "$LOG" | head -3
if [ -n "$LOGLEVEL" ]; then
    FULL=$(grep -c '按策略清残影' "$LOG" 2>/dev/null || echo 0)
    echo "  按策略触发的全屏刷新: $FULL 次"
    if [ "$FULL" -gt 0 ]; then
        echo "  ✓ vendored 的刷新策略在 ARM 二进制里生效了"
    else
        echo "  ✗ 一次全刷都没触发 —— 策略没接上？"
    fi
else
    echo "  （加 LOGLEVEL=debug 跑才能看到逐次决策）"
fi

echo
echo "=== 截图 ==="
ls -la "$SHOT"/ime-*.png 2>/dev/null || echo "  （没有截图）"

exit $RC

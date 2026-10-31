#!/bin/bash
# 封面排版的模拟器验证：起 mock-api → 起 QEMU → 进搜索页/详情页 → 量封面比例。
#
# 用的服务端是本仓库自带的 `mock-api.py`（当场生成封面，混了方形/竖/横三种比例），
# **不依赖外面那台电脑上的服务端**，也不碰真实站点。
#
# 用法：emulator/cover-test.sh
# 产出：emulator/screenshots/cover-*.png

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
LOG=/tmp/emu-cover.log
SHOT=$PROJ/emulator/screenshots
PORT=${PORT:-8001}

# mock 跑在 WSL 里 → guest 通过 slirp 网关 10.0.2.2 访问
SRV=http://10.0.2.2:$PORT

mkdir -p "$SHOT"
pkill -x qemu-system-arm 2>/dev/null
pkill -f mock-api.py 2>/dev/null
sleep 1

echo "=== 起 mock-api（端口 $PORT）==="
python3 "$PROJ/emulator/mock-api.py" "$PORT" >/tmp/mock-api.log 2>&1 &
MOCKPID=$!
cleanup() {
    kill "$MOCKPID" 2>/dev/null
    kill "${QPID:-}" 2>/dev/null
    wait 2>/dev/null
}
trap cleanup EXIT INT TERM

sleep 1
if ! curl -s -m 5 -o /dev/null "$SRV/api/info"; then
    echo "  mock-api 没起来，见 /tmp/mock-api.log"
    cat /tmp/mock-api.log
    exit 1
fi
echo "  OK"

echo
echo "=== 构建 ==="
if ! bash "$PROJ/emulator/build-emulator.sh" >/tmp/cover-build.log 2>&1; then
    echo "  构建失败，见 /tmp/cover-build.log"
    tail -25 /tmp/cover-build.log
    exit 1
fi
echo "  OK"

rm -f "$FBS" "$TSS" "$LOG"

echo
echo "=== 启动 QEMU（kdev.mode=remote，指向 mock-api）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=remote kdev.server=$SRV" \
  -display none -serial "file:$LOG" -no-reboot >/tmp/qemu-cover.err 2>&1 &
QPID=$!

for _i in $(seq 1 60); do
    [ -S "$FBS" ] && [ -S "$TSS" ] && break
    kill -0 "$QPID" 2>/dev/null || { echo "QEMU 启动失败："; cat /tmp/qemu-cover.err; exit 1; }
    sleep 0.5
done

echo
echo "=== 驱动 UI ==="
timeout 600 python3 "$PROJ/emulator/cover-test.py" "$SHOT"
RC=$?

echo
echo "=== 客户端日志 ==="
grep -aE '界面|封面|cover' "$LOG" | head -15

echo
echo "=== mock-api 收到的请求 ==="
grep -a 'GET /cover' /tmp/mock-api.log | head -8

echo
echo "=== 截图 ==="
ls -la "$SHOT"/cover-*.png 2>/dev/null || echo "  （没有截图）"

cleanup
trap - EXIT INT TERM
exit $RC

#!/bin/bash
# 端到端测试：拿着**真实服务端**（jm_kindle_raw.py）把客户端从头走一遍。
#
# 走的是真机那条路：客户端 ureq（现在是手写 HTTP）→ 局域网 → 服务端 → jmcomic。
# 和 e2e-test.sh / pagewalk-test.sh 的区别：那几个跑的是 --demo / --dir 本地模式，
# 完全不碰网络；这个必须有一个真在跑的服务端，否则没意义。
#
# 用法：
#   emulator/live-test.sh                 # 自动用 WSL 默认网关当服务端地址
#   emulator/live-test.sh 172.19.48.1     # 手动指定服务端主机
#
# 前置：电脑上跑着 jm_kindle_raw.py（默认端口 8001）。
#
# 产出：emulator/screenshots/live-*.png（首页 / 历史 / 目录 / 阅读器 / 翻页后的页）

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
LOG=/tmp/emu-live.log
SHOT=$PROJ/emulator/screenshots
PORT=${PORT:-8001}

# 服务端地址：默认取 WSL 的默认网关 —— 也就是 Windows 宿主在 WSL 里的地址。
# 注意**不能写 10.0.2.2**：那是 QEMU slirp 的网关，指的是跑 QEMU 的这台机器
# （也就是 WSL 自己），而服务端在 Windows 上。
HOSTIP=${1:-$(ip route show default | awk '{print $3}' | head -1)}
SERVER="http://$HOSTIP:$PORT"

echo "=== 服务端 $SERVER ==="
if ! curl -s -m 8 -o /dev/null -w '' "$SERVER/api/info" 2>/dev/null; then
    echo "  连不上服务端。先在电脑上跑：python jm_kindle_raw.py"
    exit 1
fi
echo "  服务端可达"

mkdir -p "$SHOT"

pkill -x qemu-system-arm 2>/dev/null
pkill -x python3 2>/dev/null
sleep 2
rm -f "$FBS" "$TSS" "$LOG" "$SHOT"/live-*.png

echo
echo "=== 构建 ==="
bash "$PROJ/emulator/build-emulator.sh" >/tmp/live-build.log 2>&1 \
    || { tail -25 /tmp/live-build.log; exit 1; }
echo "  OK"

echo
echo "=== 启动 QEMU（kdev.mode=remote）==="
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
  -append "root=/dev/vda rw rootfstype=ext4 console=ttyAMA0 rootwait loglevel=4 kdev.mode=remote kdev.server=$SERVER" \
  -display none -serial "file:$LOG" -no-reboot >/tmp/qemu-live.err 2>&1 &
QPID=$!

cleanup() { kill "$QPID" 2>/dev/null; wait "$QPID" 2>/dev/null; }
trap cleanup EXIT INT TERM

for i in $(seq 1 60); do
    [ -S "$FBS" ] && [ -S "$TSS" ] && break
    kill -0 "$QPID" 2>/dev/null || { echo "QEMU 启动失败："; cat /tmp/qemu-live.err; exit 1; }
    sleep 0.5
done

echo
echo "=== 驱动 UI ==="
timeout 600 python3 "$PROJ/emulator/live-test.py" "$SHOT"
RC=$?

cleanup
trap - EXIT INT TERM

echo
echo "=== 客户端日志 ==="
grep -E '\[INFO\]' "$LOG" | head -25

echo
echo "=== 截图 ==="
ls -la "$SHOT"/0*.png 2>/dev/null || echo "  （没有截图）"

exit $RC

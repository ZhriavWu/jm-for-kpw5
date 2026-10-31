#!/bin/bash
# 改 UI 用的开发脚本：编译桌面版并启动。
#
# ────────────────────────────────────────────────────────────────
# 为什么用桌面版改 UI
# ────────────────────────────────────────────────────────────────
# 不用碰 QEMU、不用等真机，改完编译一两秒就能看到效果。
# 界面代码（ui/app.slint）和真机共用一份，所以这里调好直接打包就是真机效果。
# 唯一的差别是电子墨水刷新速度和触摸手感 —— 那两样只能真机看。
#
# ────────────────────────────────────────────────────────────────
# 关于缩放：SLINT_SCALE_FACTOR=0.5 是必须的
# ────────────────────────────────────────────────────────────────
# ui/app.slint 里的坐标是**绝对值**，按 1236×1648 画的（比如按钮宽
# `parent.width - 160px`、阅读器热区 `parent.width * 25 / 100`）。
# 如果窗口逻辑尺寸不是 1236×1648，就只能看到左上角一块。
#
# Slint 的 winit 后端认这个环境变量：
#   逻辑尺寸 × scale = 物理尺寸
# 设成 0.5 得到：
#   逻辑 1236×1648（和 UI 坐标对齐，排版完全正确）
#   物理  618×824（1280×1024 的屏幕放得下）
#
# 觉得窗口小就把 SCALE 调大（0.75 → 927×1236），逻辑尺寸不变，排版也不变，
# 只是看得更清楚。
#
# 启动后程序会打一行「桌面窗口：物理 WxH 缩放 S 逻辑 WxH」——
# 逻辑尺寸必须是 1236×1648，不是就说明缩放没生效，排版会不对。
#
# ────────────────────────────────────────────────────────────────
# 用法
# ────────────────────────────────────────────────────────────────
#   bash kindle-manga/dev-ui.sh                    # 连服务端（默认地址）
#   bash kindle-manga/dev-ui.sh --demo             # 本地测试图，不连网
#   bash kindle-manga/dev-ui.sh --server http://192.168.1.91:8001
#   SCALE=0.75 bash kindle-manga/dev-ui.sh         # 窗口放大一点

set -uo pipefail

PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
. "$HOME/.cargo/env" 2>/dev/null || true
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/target/kindle-manga}

# 逻辑尺寸按面板算，窗口按比例缩小显示
export SLINT_SCALE_FACTOR=${SCALE:-0.5}

# 没有显式给 --server / --demo 时，用环境变量指服务端
# （和 launch.sh 里给真机用的是同一个变量，客户端本来就认它）
if [ $# -eq 0 ]; then
    export MANGA_SERVER=${MANGA_SERVER:-http://192.168.1.91:8001}
fi

cd "$PROJ" || exit 1

echo "== 编译桌面版（--features desktop）=="
# 不带 --features kindle：那个开关切到 Kindle E Ink 后端，桌面上跑不起来
# （它要找 /dev/fb0 和 EPDC ioctl）。
if ! cargo build --release --features desktop 2>&1 | tail -6; then
    echo "编译失败"
    exit 1
fi

BIN=$CARGO_TARGET_DIR/release/kindle-manga
[ -x "$BIN" ] || { echo "找不到产物 $BIN"; exit 1; }

echo
echo "== 启动 =="
echo "   缩放 $SLINT_SCALE_FACTOR（逻辑 1236x1648，和 PW5 同排版）"
[ -n "${MANGA_SERVER:-}" ] && echo "   服务端 $MANGA_SERVER"
echo "   点击左侧 25% = 上一页，右侧 25% = 下一页"
echo "   Ctrl+C 结束"
echo
exec "$BIN" "$@"

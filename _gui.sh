#!/bin/bash
# 在 WSLg 的 :0 上起桌面版 GUI，**给用户看排版**。一次性工具，用完可删。
#
# 用法：
#   bash _gui.sh                          # 自动探测服务端（WSL 默认网关）
#   bash _gui.sh http://192.168.1.91:8001 # 指定服务端
#   bash _gui.sh http://172.19.48.1:8001 0.75   # 指定缩放
#
# ⚠️ 缩放**必须给 0.5**（默认就是）：
#   桌面构建的窗口物理尺寸写死成面板的一半（618×824），
#   而 Slint 算逻辑尺寸是「物理 = 逻辑 × scale」——
#   只有 scale=0.5 才能得到逻辑 1236×1648，和 ui/*.slint 里的绝对坐标对齐。
#   给 0.75 会得到 824×1099，界面只显示左上角一块
#   （启动日志里会有「⚠️ 逻辑尺寸不是 1236x1648」，看到它就是这个原因）。
#
# ⚠️ **服务端地址不能写 10.0.2.2** —— 那是 QEMU slirp 的网关，指 WSL 自己。
#   服务端在 Windows 上，WSL 眼里的地址是**默认网关**，本脚本自动取。
#
# 这个脚本是给「让用户直接看」用的，正确的配合方式是：
#   起 GUI → 问用户「这样对不对、哪里不对」。
# 而**不是**自己截图去量像素 —— 纯排版的东西人一眼就能判断，
# 自己量要写一堆坐标推算，慢且容易量错（这上面反复栽过）。
set -u

PROJ=$(cd "$(dirname "$0")" && pwd)

# 没给服务端就自动探测：Windows 主机在 WSL 眼里是默认网关
SERVER="${1:-}"
if [ -z "$SERVER" ]; then
    GW=$(ip route show default | awk '{print $3}')
    SERVER="http://${GW}:8001"
    echo "自动探测服务端: $SERVER"
fi
SCALE="${2:-0.5}"

export DISPLAY="${DISPLAY:-:0}"
# dev-ui.sh 读的是 SCALE 这个环境变量（它内部再转成 SLINT_SCALE_FACTOR）
export SCALE

# ⚠️ **先杀掉还在跑的旧实例。**
#
# 不然桌面上会堆着好几个窗口（每次重编译都起一个新的），
# 而 `xdotool search --name kindle-manga` 只返回**第一个** ——
# 那个多半是旧构建，于是自动化点击全打在旧窗口上，
# 看起来像「改动没生效」。这个坑踩过一次，白查了半天。
#
# 用 `pkill -x`（精确匹配进程名）：**绝对不要用 `pkill -f`** ——
# 命令行里含那个字符串的话会把自己的 shell 一起杀掉。
pkill -x kindle-manga 2>/dev/null && { echo "已清掉旧实例"; sleep 1; }

cd "$PROJ" || exit 1
exec bash dev-ui.sh --server "$SERVER"

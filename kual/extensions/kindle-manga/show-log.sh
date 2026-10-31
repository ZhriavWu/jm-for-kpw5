#!/bin/sh
# 在 Kindle 屏幕上显示最近一次运行的日志末尾。
#
# 为什么用 eips 而不是打开文件：Kindle 上没装终端，出问题时最方便的排错方式
# 就是把日志直接打到屏幕上拍照。eips 是 Kindle 自带的帧缓冲文本工具。
#
# ⚠️ 这个脚本存在的意义是「当应用起不来时，唯一的现场线索」。
# 所以它自己必须尽量不会哑火：eips 的路径在不同固件上不一样，
# 这里挨个试；实在找不到就把要说的话写到 USB 上的文件里，
# 插回电脑还能看。

LOGDIR=/mnt/us/kindle-manga-logs
FALLBACK=/mnt/us/kindle-manga-logs/屏幕提示.txt

# ---- 先找 eips：不同固件放在不同位置，别赌 PATH ----
EIPS=""
for p in /usr/sbin/eips /usr/bin/eips /sbin/eips /bin/eips /usr/local/bin/eips; do
    [ -x "$p" ] && { EIPS="$p"; break; }
done
[ -z "$EIPS" ] && EIPS=$(command -v eips 2>/dev/null)

show() { [ -n "$EIPS" ] && "$EIPS" "$@" 2>/dev/null; }

# eips 找不到时的兜底：把内容留在 USB 上，插回电脑能读
save_fallback() {
    {
        echo "===== 屏幕提示（eips 不可用时的兜底）====="
        echo "eips: ${EIPS:-没找到}"
        echo "时间: $(date)"
        echo ""
        echo "$1"
    } >>"$FALLBACK" 2>/dev/null
}

mkdir -p "$LOGDIR" 2>/dev/null

if [ ! -d "$LOGDIR" ]; then
    show -c
    show 2 2 "还没有日志目录 —— 说明启动脚本没跑过"
    save_fallback "日志目录不存在，启动脚本可能从未执行"
    exit 0
fi

LATEST=$(ls -t "$LOGDIR"/*.log 2>/dev/null | head -1)

if [ -z "$LATEST" ]; then
    show -c
    show 2 2 "日志目录为空 —— 启动脚本没跑过"
    save_fallback "日志目录为空"
    exit 0
fi

# ---- 画到屏幕上 ----
show -c
show 1 1 "日志: $(basename "$LATEST")"
show 1 2 "eips: ${EIPS:-无}   时间: $(date +%H:%M:%S)"

# 屏幕一行大约能放 60 个字符。取末尾 32 行 —— 出问题时最要紧的是最后几步。
LINE=4
tail -32 "$LATEST" | while IFS= read -r L; do
    SHORT=$(printf '%s' "$L" | cut -c1-72)
    show 1 $LINE "$SHORT"
    LINE=$((LINE + 1))
done

show 1 40 "日志文件: kindle-manga-logs/"

# ---- 同时留一份到 USB，插回电脑也能看 ----
save_fallback "$(tail -60 "$LATEST")"

#!/bin/sh
# 最小自检：只做「能不能写文件」和「能不能跑二进制」，把结果留到 USB 上。
#
# 为什么要单独有这么一个：
#   「点菜单没反应」有两种完全不同的原因，得先分清是哪一种 ——
#     1. KUAL 根本没能执行我们的脚本（权限位、格式、固件差异）
#     2. 脚本执行了，但里面某一步失败
#   这个脚本做的第一件事就是往 USB 写一个文件。只要那个文件出现了，
#   就证明 KUAL **能**跑我们的脚本，问题在脚本内部；没出现，就是 1。
#
# 结果全部留在 /mnt/us/kual-selftest.txt，插回电脑直接看，不依赖 eips。

OUT=/mnt/us/kual-selftest.txt

# 第一件事：证明「我跑起来了」。哪怕后面全挂，这一条也能证明脚本被执行了。
echo "=== KUAL 能执行自检脚本 ===" >"$OUT" 2>&1
echo "时间: $(date)" >>"$OUT" 2>&1
echo "脚本: $0" >>"$OUT" 2>&1
echo "PID: $$" >>"$OUT" 2>&1
echo "" >>"$OUT" 2>&1

{
    echo "--- 环境 ---"
    echo "机型: $(cat /proc/device-tree/model 2>/dev/null | tr -d '\0')"
    echo "固件: $(cat /etc/version.txt 2>/dev/null | head -1)"
    echo "内核: $(uname -r)"
    echo "PATH: $PATH"
    echo ""

    echo "--- eips 在哪 ---"
    for p in /usr/sbin/eips /usr/bin/eips /sbin/eips /bin/eips; do
        [ -x "$p" ] && echo "  找到: $p"
    done
    command -v eips >/dev/null 2>&1 && echo "  PATH 里也有: $(command -v eips)" || echo "  PATH 里没有"
    echo ""

    echo "--- 常用命令有没有（busybox 被裁过，得确认）---"
    for c in killall pidof lipc-set-prop usleep ping wget sed awk grep head tail cut; do
        if command -v "$c" >/dev/null 2>&1; then
            echo "  有   $c"
        else
            echo "  缺   $c   ← 缺的话 launch.sh 里用到它的地方会静默失败"
        fi
    done
    echo ""

    echo "--- 帧缓冲与输入设备 ---"
    echo "/proc/fb:"
    cat /proc/fb 2>/dev/null || echo "  (读不到)"
    echo "/dev/fb*:"
    ls -la /dev/fb* 2>/dev/null || echo "  (没有)"
    echo "/dev/input/:"
    ls /dev/input/ 2>/dev/null || echo "  (没有)"
    echo ""

    echo "--- 抢屏幕的进程（名字对不对，决定 launch.sh 能不能释放屏幕）---"
    for p in awesome cvm KPPMainApp reader.lua pillow framework; do
        echo "  $p: $(pidof "$p" 2>/dev/null || echo '（没在跑）')"
    done
    echo ""

    echo "--- 电源相关进程 ---"
    for p in powerd pillow framework; do
        echo "  $p: $(pidof "$p" 2>/dev/null || echo '（没在跑）')"
    done
    echo ""

    echo "--- 输入设备表（找电源键是哪一号 event）---"
    cat /proc/bus/input/devices 2>/dev/null | head -50 || echo "  (读不到)"
    echo ""

    echo "--- 谁持有输入设备（电源键的持有者很可能就是负责休眠的那个）---"
    for pid in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$'); do
        comm=$(cat "/proc/$pid/comm" 2>/dev/null)
        for fd in /proc/$pid/fd/*; do
            link=$(readlink "$fd" 2>/dev/null) || continue
            case "$link" in
                */input/*) echo "  pid=$pid comm=$comm -> $link" ;;
            esac
        done
    done
    echo ""

    echo "--- 恢复界面要用到的工具（退出后防白屏）---"
    echo "  DISPLAY 环境变量 = ${DISPLAY:-未设置}"
    for p in /usr/bin/xrefresh /usr/local/bin/xrefresh; do
        [ -x "$p" ] && echo "  ✓ xrefresh: $p"
    done
    command -v xrefresh >/dev/null 2>&1 && echo "  ✓ PATH 里也有 xrefresh" || echo "  PATH 里没有 xrefresh"
    echo "  /dev/fb0 可读 = $([ -r /dev/fb0 ] && echo 是 || echo 否)（备份画面要用）"
    echo "  试跑 xrefresh（只是重绘当前界面，安全、无副作用）："
    for d in :0.0 :0; do
        if DISPLAY=$d timeout 10 xrefresh >/dev/null 2>&1; then
            echo "    ✓ DISPLAY=$d 成功 —— 退出后防白屏这条能生效"
            break
        else
            echo "    ✗ DISPLAY=$d 失败"
        fi
    done
    echo ""

    echo "--- 电源/休眠相关 sysfs ---"
    for f in /sys/power/state /sys/power/wakeup_count /sys/power/autosleep /sys/power/wake_lock; do
        echo "  $f = $(cat "$f" 2>&1 | head -1)"
    done
    echo "  /sys/power 下有哪些文件: $(ls /sys/power 2>/dev/null | tr '\n' ' ')"
    for n in 0 1 2; do
        [ -e "/sys/class/rtc/rtc$n/wakealarm" ] && echo "  rtc$n wakealarm = $(cat /sys/class/rtc/rtc$n/wakealarm 2>&1)"
    done
    echo ""

    echo "--- powerd 的 LIPC 属性（判断 preventScreenSaver 存不存在）---"
    echo "  preventScreenSaver = $(lipc-get-prop com.lab126.powerd preventScreenSaver 2>&1)"
    echo "  powerd state       = $(lipc-get-prop com.lab126.powerd state 2>&1)"
    echo ""

    echo "--- 谁持有 wake lock（/sys/kernel/debug/wakeup_sources）---"
    if [ -r /sys/kernel/debug/wakeup_sources ]; then
        head -20 /sys/kernel/debug/wakeup_sources
    else
        echo "  读不到（debugfs 没挂）"
    fi
    echo ""

    echo "--- 扩展目录 ---"
    ls -la /mnt/us/extensions/kindle-manga/ 2>/dev/null
    echo ""

    echo "--- 二进制能不能执行 ---"
    BIN=/mnt/us/extensions/kindle-manga/kindle-manga
    if [ -f "$BIN" ]; then
        echo "  存在，$(wc -c <"$BIN" 2>/dev/null) 字节"
        echo "  文件头: $(head -c 4 "$BIN" 2>/dev/null | od -An -tx1 | tr -d ' ')  (ELF 应为 7f454c46)"
        [ -x "$BIN" ] && echo "  有执行位" || echo "  没有执行位 ← KUAL 可能因此跑不起来"
    else
        echo "  ✗ 不存在"
    fi
    echo ""

    echo "--- 网络 ---"
    echo "本机 IP: $(ip -4 addr show 2>/dev/null | grep -oE 'inet [0-9.]+' | awk '{print $2}' | tr '\n' ' ')"
    echo ""

    echo "--- 试跑二进制的自检模式（不建窗口，安全）---"
    if [ -x "$BIN" ]; then
        # --discover 只发个 UDP 广播，秒回，不会接管屏幕
        MANGA_SERVER=http://192.168.1.91:8001 "$BIN" --discover 2>&1 | head -20
        echo "  （上面是 --discover 的输出）"
    else
        echo "  跳过：二进制不可执行"
    fi
} >>"$OUT" 2>&1

echo "" >>"$OUT" 2>&1
echo "=== 自检结束，把 kindle 连电脑看 kual-selftest.txt ===" >>"$OUT" 2>&1

# 屏幕上也给个提示（eips 可用的话）
EIPS=""
for p in /usr/sbin/eips /usr/bin/eips /sbin/eips /bin/eips; do
    [ -x "$p" ] && { EIPS="$p"; break; }
done
[ -z "$EIPS" ] && EIPS=$(command -v eips 2>/dev/null)
if [ -n "$EIPS" ]; then
    "$EIPS" -c 2>/dev/null
    "$EIPS" 1 1 "自检完成" 2>/dev/null
    "$EIPS" 1 3 "结果在 kual-selftest.txt" 2>/dev/null
    "$EIPS" 1 5 "把 kindle 接电脑就能看" 2>/dev/null
fi

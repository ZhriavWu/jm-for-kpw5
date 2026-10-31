#!/bin/sh
# KUAL 启动脚本：暂停 Kindle 的 UI 框架 → 把屏幕交给我们的应用 → 退出后恢复。
#
# 为什么必须暂停框架：Kindle 的阅读器进程会持续往帧缓冲和 EPDC 写内容，
# 不暂停的话两边抢屏幕，画面会撕裂或闪烁。
#
# 用 -STOP / -CONT 而不是 kill：停止的进程仍然占着已打开的设备句柄，
# 恢复后能接着用；杀掉的话很多服务起不回来，只能重启设备。
#
# ────────────────────────────────────────────────────────────────
# 两条硬规矩，改这个脚本时别忘：
#
# 1. **所有输出必须 ASCII。**
#    eips 画不了非 ASCII 字符，写中文上去屏幕只会显示
#    "paint_char> character not available"。屏幕是出事时唯一的现场反馈，
#    所以这里的每一条提示都得是英文。
#
# 2. **绝不能漏掉恢复框架。**
#    脚本开头把框架 -STOP 了，如果中途出错就走了，那三行 -CONT 永远执行不到，
#    框架一直冻着，设备看起来就像砖了 —— 只能长按强制重启。
#    所以下面用 trap 兜住所有退出路径（正常结束、报错退出、被 Ctrl-C、被 TERM）。

DIR=/mnt/us/extensions/kindle-manga
BIN="$DIR/kindle-manga"
LOGDIR=/mnt/us/kindle-manga-logs
TS=$(date +%Y%m%dT%H%M%S)
LOG="$LOGDIR/$TS.log"

mkdir -p "$LOGDIR" 2>/dev/null

# 从这里开始，所有输出（含错误）都进日志
exec >>"$LOG" 2>&1

# ================================================================ 工具函数
#
# 先把基础工具准备好，再定义恢复逻辑 —— 恢复函数要用到 EIPS，
# 它的值必须在 trap 挂上之前就已经确定。

STEP=0
T0=$(date +%s)

# 每一步都记「从脚本开始到现在过了几秒」。
#
# 加这个是因为用户反馈「启动要等十几秒且没有任何提示」。光靠日志看不出
# 时间花在哪一步 —— 每行都有时间戳才知道。开销极小（每次一次 date）。
mark() {
    STEP=$((STEP + 1))
    NOW=$(date +%s)
    echo ""
    echo "### [${STEP}] $*   (+$((NOW - T0))s) ###"
}

# eips 路径在不同固件上不一样，挨个找，别赌 PATH
EIPS=""
for p in /usr/sbin/eips /usr/bin/eips /sbin/eips /bin/eips /usr/local/bin/eips; do
    [ -x "$p" ] && { EIPS="$p"; break; }
done
[ -z "$EIPS" ] && EIPS=$(command -v eips 2>/dev/null)

show() { [ -n "$EIPS" ] && "$EIPS" "$@" 2>/dev/null; }

# ------------------------------------------------------------------ 帧缓冲备份
#
# **为什么必须备份屏幕**：我们是直接往 /dev/fb0 写的，绕过了 X。
# X 一直以为自己的窗口还在屏幕上，其实早被我们盖掉了。等我们退出时，
# 帧缓冲里剩的是我们的最后一帧（或者被 eips -c 清成白的）——
# 而 X 毫不知情，所以**不会重绘**。表现就是：退出后回到 KUAL，但整屏是白的，
# 只有在你点了某个地方、X 收到事件之后，那个窗口才补画出来。
#
# 两道保险一起上：
#   1. 开跑之前把当前画面整份存下来，退出时原样贴回去 —— 瞬间恢复，不会白屏
#   2. 再用 xrefresh 让 X 把整屏重画一遍 —— 这样恢复出来的是**活的**界面
#      （时钟、状态栏都是当前的），而不是一张陈年截图
#
# 第 2 条是主菜，第 1 条是它万一失败时的兜底。xrefresh 是固件自带的
# （/usr/bin/xrefresh），不用另装。KOReader 用的是备份帧缓冲这一招
# （它的 koreader.sh 里 `cat /dev/fb0 >/var/tmp/koreader-fb.dump`），
# 我们两个都用。
FB_DUMP=/var/tmp/kindle-manga-fb.dump

# xrefresh 在 /usr/bin，但别赌 PATH
XREFRESH=""
for p in /usr/bin/xrefresh /usr/local/bin/xrefresh; do
    [ -x "$p" ] && { XREFRESH="$p"; break; }
done
[ -z "$XREFRESH" ] && XREFRESH=$(command -v xrefresh 2>/dev/null)

# 让 X 重绘整屏。
#
# 原理：xrefresh 相当于对 X 说「整块屏幕都暴露了，把所有窗口重画一遍」。
# 这正是「用手点一下」会触发的事情，只是它覆盖全屏、瞬时完成。
#
# DISPLAY 要显式给：脚本跑在 KUAL 的环境里，不一定有 X 的显示变量。
# `:0.0` 是固件自己的写法（/etc/upstart/env 里就是 `export DISPLAY=:0.0`），
# 所以先试它，再退回 `:0`。这台机器不需要 Xauthority（X 和我们的脚本都是 root）。
force_repaint() {
    if [ -z "$XREFRESH" ]; then
        echo "  xrefresh 不存在，跳过（只能靠帧缓冲备份兜底）"
        return 1
    fi

    for d in :0.0 :0; do
        echo "  执行 $XREFRESH（DISPLAY=$d）"
        # 加超时：万一 X 没响应，不能把退出流程卡在这儿
        if DISPLAY=$d timeout 10 "$XREFRESH" 2>&1; then
            echo "  X 已重绘 ✓（DISPLAY=$d）"
            return 0
        fi
        echo "  DISPLAY=$d 不行，换一个试"
    done

    echo "  ⚠️ xrefresh 在所有 display 上都失败了 —— 只能靠帧缓冲备份兜底"
    return 1
}

# ================================================================ 恢复框架
#
# 必须在做任何破坏性动作**之前**定义好并用 trap EXIT 挂上，
# 这样无论脚本怎么结束都会跑到。RESTORED 保证幂等 ——
# 正常走到末尾、出错退出、被 Ctrl-C、被 TERM，都只会执行一次。

RESTORED=0

# 内置 Go 服务端的 PID。为空 = 这次没起（或起失败）。
#
# ⚠️ 必须在 trap 挂上**之前**就定义好：restore_framework 会在任何退出路径上
#    读它。定义晚了一步的话，脚本在启动服务端之前出错退出时 `$SRV_PID`
#    是未定义的（sh 里带 set -u 会直接报错，不带就是在 `[ -n "" ]` 上蒙对）。
SRV_PID=""

restore_framework() {
    [ "$RESTORED" = 1 ] && return 0
    RESTORED=1

    echo ""
    echo "### RESTORE FRAMEWORK (trap) ###"

    # 0) 先停内置服务端。
    #    放在恢复框架**之前**：它还在下载的话会继续占着 WiFi 和 CPU，
    #    而接下来要重绘整个界面，越早放手越好。
    if [ -n "$SRV_PID" ]; then
        echo "stop built-in server (pid $SRV_PID)"
        kill "$SRV_PID" 2>/dev/null
        wait "$SRV_PID" 2>/dev/null
    fi

    # 1) 让框架进程继续跑
    for p in KPPMainApp cvm awesome; do
        killall -CONT "$p" 2>/dev/null
    done
    # 把 powerd 的自动屏幕保护交还给它（下面 STOP 那边关掉的）。
    # 这一步漏了的话，Kindle 自己的休眠就永久废了 —— 所以它由 trap 保证执行。
    echo "restore preventScreenSaver=$(lipc-set-prop com.lab126.powerd preventScreenSaver 0 2>&1 || echo '失败')"
    lipc-set-prop com.lab126.pillow disableEnablePillow enable 2>/dev/null

    # 给框架一点时间真的跑起来。
    # 它们被 SIGSTOP 冻了挺久，恢复后要处理积压的事件、重新画窗口，
    # 立刻 xrefresh 可能会在它们还没准备好时把重绘请求发出去。
    sleep 1

    # 2) 全屏闪一下清掉墨水残影。
    #    放在贴回画面**之前** —— 先清干净，再往上画。
    [ -n "$EIPS" ] && "$EIPS" -f -c 2>/dev/null

    # 3) 兜底：把开跑前备份的画面贴回去。
    #    万一 xrefresh 失败，至少用户看到的不是白屏，而是他进去之前那一屏。
    if [ -f "$FB_DUMP" ]; then
        if cat "$FB_DUMP" >/dev/fb0 2>/dev/null; then
            echo "  已贴回备份画面（$(stat -c%s "$FB_DUMP" 2>/dev/null) 字节）"
        else
            echo "  ⚠️ 贴回备份画面失败"
        fi
    else
        echo "  （没有帧缓冲备份可用）"
    fi

    # 4) 主菜：让 X 重绘整屏，把界面的**当前状态**画上去。
    #    少了这一步，用户看到的就是一张定格的旧图 —— 点一下才会活过来。
    force_repaint

    sync
    echo "framework restored"
    sleep 1
}

trap restore_framework EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# 打印一组进程的真实状态。
#
# 为什么要读 /proc/PID/status 而不是只看 PID 在不在：
# `killall -STOP` 是**暂停**不是杀死，PID 当然还在 ——
# 只列 PID 会让人误以为「没停成功」。State 字段里 T 才是真的停了。
proc_state() {
    for name in "$@"; do
        pid=$(pidof "$name" 2>/dev/null | awk '{print $1}')
        if [ -z "$pid" ]; then
            echo "    $name: not running"
        else
            st=$(awk '/^State:/{print $2}' "/proc/$pid/status" 2>/dev/null)
            echo "    $name: pid=$pid state=${st:-?}   (T=stopped S=sleeping R=running)"
        fi
    done
}

# 谁打开了输入设备。
#
# 这是为了定位「电源键不响应」：电源键本质是一个 evdev 设备，
# 某个进程读它、然后决定休眠。把持有者列出来，就能知道
# 我们 -STOP 的那几个进程里，是不是有负责电源键的。
#
# ⚠️ **这个函数很慢**：要遍历 /proc 下每个进程的每一个 fd，是 O(进程数 × fd 数)
# 次 readlink。Kindle 上开机跑着上百个进程，这一项实测要好几秒 ——
# 而它原来在脚本里被调用**两次**，正好卡在用户盯着空屏等的那段路里。
#
# 它只在「查电源键被谁占着」这一件事上有用，那是**一次性的排障**，
# 日常启动完全不需要。所以默认不跑，要看就设 MANGA_VERBOSE=1。
input_holders() {
    [ "${MANGA_VERBOSE:-0}" = "1" ] || {
        echo "    （跳过：input_holders 很慢，设 MANGA_VERBOSE=1 才跑）"
        return 0
    }
    for pid in $(ls /proc 2>/dev/null | grep -E '^[0-9]+$'); do
        comm=$(cat "/proc/$pid/comm" 2>/dev/null)
        for fd in /proc/$pid/fd/*; do
            link=$(readlink "$fd" 2>/dev/null) || continue
            case "$link" in
                */input/event*|*/input/mice|*/input/uinput)
                    echo "    pid=$pid comm=$comm -> $link"
                    ;;
            esac
        done
    done
}

echo "================================================"
echo "JMcomic for kindle"
echo "time: $(date)"
echo "log : $LOG"
echo "================================================"

# ================================================================ 0. 立刻给反馈
#
# **这一段必须在所有诊断之前。**
#
# 用户反馈「点完菜单要盯着原界面十几秒，没有任何提示，然后才闪一下」。
# 根因就是：原来第一条屏幕消息（`show 2 2 "Starting..."`）排在 FRAMEBUFFER /
# INPUT / CHECK BINARY 那几节诊断**之后**，而下面 `input_holders` 要遍历
# /proc 下每个进程的每一个 fd —— 光这一项就要好几秒。
# 那段时间屏幕上一直是 KUAL 的菜单，用户完全不知道发生了什么。
#
# 所以顺序改成：**先备份画面 → 立刻把提示画上去 → 再做诊断**。
mark "EARLY FEEDBACK"

# 备份必须在**画提示之前**做 —— 备份的是「用户此刻看到的正常界面」，
# 退出时用它兜底。要是先画了提示，备份下来的就是自己的提示了。
echo "--- 备份当前画面 ---"
if [ -r /dev/fb0 ]; then
    if cat /dev/fb0 >"$FB_DUMP" 2>/dev/null; then
        echo "  已备份到 $FB_DUMP（$(stat -c%s "$FB_DUMP" 2>/dev/null) 字节）"
    else
        echo "  ⚠️ 备份失败，退出时只能靠 xrefresh"
    fi
else
    echo "  /dev/fb0 读不了，跳过备份"
fi

if [ -n "$EIPS" ]; then
    "$EIPS" -c 2>/dev/null
    "$EIPS" 4 4 "JMcomic for kindle" 2>/dev/null
    "$EIPS" 8 4 "Starting, please wait..." 2>/dev/null
    echo "  已把提示画到屏幕上"
else
    echo "  ⚠️ 没找到 eips，这段时间屏幕上不会有提示"
fi

# ================================================================ 1. 环境
mark "ENV"
echo "model : $(cat /proc/device-tree/model 2>/dev/null | tr -d '\0')"
echo "firm  : $(cat /etc/version.txt 2>/dev/null | head -1)"
echo "kernel: $(uname -r)"
echo "eips  : ${EIPS:-NOT FOUND}"
echo "PATH  : $PATH"

# ================================================================ 2. 服务端
#
# 现在有**两种**服务端，客户端不用改一行：
#
#   A. 内置（默认）—— 本目录下的 `kindle-manga-server`，只监听 127.0.0.1。
#      整套东西只依赖 Kindle 自己 + WiFi，不需要电脑常开。
#   B. 电脑上的 Python 版 —— 用户在「设置 → 服务器地址」里手填（存进 server.txt）。
#
# 客户端选地址的优先级是：`--server` > `server.txt` > `MANGA_SERVER` 环境变量。
# 所以这里把内置地址放进 **MANGA_SERVER**（最低优先级）：
#
#   * 没填过地址 → 用内置（开箱即用）
#   * 填了电脑地址 → 用电脑；电脑没开时探活失败，会自动落到内置（兜底）
#   * 想换回内置 → 在设置里把地址清空（留空即「回到自动发现」）
mark "SERVER ADDRESS"

SRV_BIN="$DIR/kindle-manga-server"
SRV_READY=/var/tmp/kindle-manga-server.ready
SRV_LOG="$LOGDIR/server-$TS.log"

if [ -z "${MANGA_SERVER:-}" ]; then
    MANGA_SERVER=http://127.0.0.1:8001
fi
export MANGA_SERVER
echo "MANGA_SERVER=$MANGA_SERVER"
echo "this device IP: $(ip -4 addr show 2>/dev/null | grep -oE 'inet [0-9.]+' | awk '{print $2}' | tr '\n' ' ')"

# 起内置服务端。
#
# 三种情况跳过：
#   1. 没有这个二进制（用户拿到的是纯客户端包）→ 退回局域网模式
#   2. 端口已经有响应（上一次没退干净）→ 复用，不重复起
#   3. 用户填了外部地址 → 仍然起，当兜底（内存代价约 15 MB，值得）
if [ ! -f "$SRV_BIN" ]; then
    echo "no built-in server binary, skip (LAN mode)"
    show 16 4 "No built-in server, using LAN..."
else
    [ -x "$SRV_BIN" ] || chmod 755 "$SRV_BIN" 2>/dev/null

    # 复用已经在跑的那个（上次异常退出留下的）。
    #
    # ⚠️ **不能用 pidof kindle-manga-server**：Linux 的进程名（/proc/PID/comm）
    #    只有 15 个字符，"kindle-manga-server" 会被截成 "kindle-manga-se"，
    #    pidof 按完整名字找会永远找不到。改用就绪文件里记的 PID。
    REUSE=0
    if [ -f "$SRV_READY" ]; then
        OLD=$(cat "$SRV_READY" 2>/dev/null)
        if [ -n "$OLD" ] && kill -0 "$OLD" 2>/dev/null; then
            echo "built-in server already running (pid $OLD), reuse"
            SRV_PID="$OLD"
            REUSE=1
        else
            echo "stale ready file (pid ${OLD:-?} not alive), restart"
            rm -f "$SRV_READY"
        fi
    fi

    if [ "$REUSE" = 1 ]; then
        :
    else
        rm -f "$SRV_READY"

        # 缓存上限：用户可以在设置里改，值存在 cache_limit.txt。
        # 这里是**启动那一刻**读一次；运行中改的话客户端会走
        # POST /api/cache 通知这个进程（见 src/main.rs 的 push_cache_limit）。
        #
        # ⚠️ 校验要**严格**：整份内容必须是一个纯数字。
        #    原来用 `tr -cd '0-9'` 抽数字，于是被改坏的 "12x8" 会变成 128 ——
        #    一个用户没设过的值。宁可整个作废回默认，也别猜他想说什么。
        CACHE_MB=$(cat "$DIR/cache_limit.txt" 2>/dev/null | tr -d ' \t\r\n')
        case "$CACHE_MB" in
            ''|*[!0-9]*) CACHE_MB=1024 ;;
        esac
        # 去掉前导 0（"0512" 在某些 shell 的 [ -gt ] 里会被当八进制）
        CACHE_MB=$(echo "$CACHE_MB" | sed 's/^0*//')
        [ -z "$CACHE_MB" ] && CACHE_MB=0
        # 大于 64 GB 一律当没写（文件被改坏时别变成「不限制」）
        [ "$CACHE_MB" -gt 65536 ] 2>/dev/null && CACHE_MB=1024

        echo "start built-in server: $SRV_BIN (cache max ${CACHE_MB}MB)"
        "$SRV_BIN" \
            -addr 127.0.0.1:8001 \
            -cache /mnt/us/kindle-manga-cache \
            -history "$DIR/history.json" \
            -ready "$SRV_READY" \
            -cache-max-mb "$CACHE_MB" \
            >>"$SRV_LOG" 2>&1 &
        SRV_PID=$!
        echo "server pid=$SRV_PID log=$SRV_LOG"

        # 等内容就绪。最多等 20 秒 —— 首次启动要更新域名（一次网络往返）。
        i=0
        while [ $i -lt 20 ]; do
            [ -f "$SRV_READY" ] && break
            # 进程已经死了就别等了
            kill -0 "$SRV_PID" 2>/dev/null || break
            sleep 1
            i=$((i + 1))
        done

        if [ -f "$SRV_READY" ]; then
            echo "server ready after ${i}s"
            show 16 4 "Server ready."
        else
            echo "WARN: server not ready after ${i}s (see $SRV_LOG)"
            show 16 4 "Server slow, starting anyway..."
        fi
    fi
fi

# ================================================================ 3. 设备
#
# 整节都是**排障用的**，而且每一条都要么开进程、要么读 /proc，加起来是启动
# 路径上第二笔开销（第一笔是 input_holders）。日常启动不需要，所以默认只留
# 最便宜的两条（/proc/fb 和 /dev/input 列表），其余等 MANGA_VERBOSE=1。
mark "FRAMEBUFFER / INPUT"
echo "/proc/fb:"
cat /proc/fb 2>/dev/null || echo "  (cannot read)"
echo "/dev/input/:"
ls /dev/input/ 2>/dev/null || echo "  (none)"

if [ "${MANGA_VERBOSE:-0}" = "1" ]; then
    echo "/dev/fb*:"
    ls -la /dev/fb* 2>/dev/null || echo "  (none)"

    echo ""
    echo "--- input device table (find the power button) ---"
    cat /proc/bus/input/devices 2>/dev/null | head -60

    echo ""
    echo "--- power related processes ---"
    for p in powerd pillow framework awesome cvm KPPMainApp; do
        pid=$(pidof "$p" 2>/dev/null)
        echo "    $p: ${pid:-not running}"
    done
else
    echo "（省略设备详表 / 进程表：设 MANGA_VERBOSE=1 展开）"
fi

echo ""
echo "--- who holds input devices (BEFORE stopping) ---"
input_holders

# ================================================================ 4. 二进制
mark "CHECK BINARY"
if [ ! -f "$BIN" ]; then
    echo "FATAL: binary not found: $BIN"
    show -c
    show 2 2 "Binary not found"
    exit 1
fi
ls -la "$BIN"
echo "magic: $(head -c 4 "$BIN" 2>/dev/null | od -An -tx1 | tr -d ' ')  (ELF=7f454c46)"

if [ ! -x "$BIN" ]; then
    echo "no exec bit, trying chmod"
    chmod 755 "$BIN" 2>/dev/null
fi
if [ ! -x "$BIN" ]; then
    echo "FATAL: binary not executable (mount may be noexec)"
    show -c
    show 2 2 "Binary not executable"
    exit 1
fi

# ================================================================ 5. 让出屏幕
mark "STOP FRAMEWORK"
# 提示在最开头就已经画上去了（见「0. 立刻给反馈」那一节），这里只更新一行。
# 不要再 `show -c` —— 多一次全屏清空就多几百毫秒的墨水刷新。
show 12 4 "Preparing screen..."

if pidof reader.lua >/dev/null 2>&1; then
    echo "stopping KOReader first"
    kill $(pidof reader.lua) 2>/dev/null
    sleep 1
fi

echo "lipc: disable pillow"
lipc-set-prop com.lab126.pillow disableEnablePillow disable 2>/dev/null

# 把 powerd 的「自动屏幕保护」关掉。
#
# 为什么：电源键和合盖休眠的链路是 powerd → 问框架（com.lab126.kaf，由 cvm 提供）
# 「框架起来了没」→ 再决定休眠。而框架正要被我们冻住，powerd 那个同步读会等满
# 10 秒超时，于是它判定「还在开机 splash」—— 电源键被它忽略；合盖时它又卡在
# SCREEN SAVER 状态里出不来，同时吞掉触摸。两个症状一个根因。
#
# 关掉之后 powerd 不再管这条状态机，改由应用自己读 evdev 处理电源键和合盖
# （见 src/power.rs）。**注意要在退出时恢复**，否则 Kindle 自己的休眠就永久废了 ——
# 恢复那步在 restore_framework 里，由 trap 保证一定会跑到。
echo "lipc: 关掉 powerd 的自动屏幕保护（改由应用自己处理电源键）"
# 不要 2>/dev/null —— 属性名写错的话 lipc 会报错，而那是这条修复是否生效的
# 唯一判据。出错还被静默吞掉的话，就变成「合盖卡死没修好但不知道为什么」。
if lipc-set-prop com.lab126.powerd preventScreenSaver 1 2>&1; then
    echo "  set 成功"
else
    echo "  ⚠️ set 失败 —— 属性名可能不对，合盖的修复不会生效"
fi
# 回读一次：属性存在的话能读回值，这是最硬的确认
echo "  回读 preventScreenSaver = $(lipc-get-prop com.lab126.powerd preventScreenSaver 2>&1)"

# 画面备份在「0. 立刻给反馈」那一节就做完了（那时屏幕还是干净的 KUAL 界面），
# 不放这里 —— 放这里的话上面那行 "Preparing screen..." 已经画上去了，
# 备份下来的就成了我们自己的提示文字，退出时贴回去等于把提示留在屏幕上。

# 要暂停哪些进程。默认和 slint-backend-kindle 的官方示例一致。
# 留这个变量是为了方便单独实验 —— 比如只想停 awesome 看电源键会不会恢复。
STOP_LIST=${MANGA_STOP_LIST:-"awesome cvm KPPMainApp"}

for p in $STOP_LIST; do
    echo "killall -STOP $p"
    killall -STOP "$p" 2>/dev/null
done

usleep 300000 2>/dev/null || sleep 1

echo ""
echo "--- process state AFTER stopping ---"
proc_state awesome cvm KPPMainApp

echo ""
echo "--- input holders AFTER stopping ---"
input_holders

# ================================================================ 6. 跑应用
mark "RUN APP"
cd "$DIR" || { echo "FATAL: cd $DIR failed"; exit 1; }
echo "cwd: $(pwd)"
echo "start: $(date +%s)"
echo "---- app output begin ----"

"$BIN"
RC=$?

echo "---- app output end ----"
echo "exit code: $RC"
echo "end: $(date +%s)"

if [ "$RC" -ge 128 ]; then
    echo "=> killed by signal $((RC - 128))"
elif [ "$RC" -ne 0 ]; then
    echo "=> non-zero exit, see app output above"
else
    echo "=> normal exit"
fi

# ================================================================ 7. 恢复
mark "EXIT"
# 实际恢复由 EXIT trap 做（restore_framework），这里只是留个标记
echo "leaving; trap will restore the framework"
echo "log saved to: $LOG"

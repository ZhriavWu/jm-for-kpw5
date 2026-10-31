#!/bin/busybox sh
# 在 Kindle 环境里启动应用（模拟专用）。
#
# 这是 initramfs 切换根目录之后执行的「用户态 init」。真机上这一步由 Amazon
# 自己的 init 完成，模拟环境里我们只需要把伪文件系统挂好、把 virtio-serial
# 端口准备好，然后带着 shim 跑应用。
#
# 面板尺寸必须和 fbshim 的 FB_W/FB_H 一致：后端拿 FBIOGET_VSCREENINFO 返回的
# xres/yres 当屏幕尺寸，shim 默认是 1272x1696（不是 PW5 的尺寸），不覆盖就错。

export PATH=/bin:/sbin:/usr/bin:/usr/sbin
BB=/bin/busybox
MNT=/bin/mount

PANEL_W=${PANEL_W:-1236}
PANEL_H=${PANEL_H:-1648}

# ---- 伪文件系统兜底（initramfs 里已经 move 过来，这里防漏）----
$MNT -n -t proc     proc /proc 2>/dev/null
$MNT -n -t sysfs    sys  /sys  2>/dev/null
$MNT -n -t devtmpfs dev  /dev  2>/dev/null

# 注意顺序：/tmp 是指向 /var/tmp 的软链接，但 /var/tmp 在镜像里并不存在
# （真机上由 Kindle 的 init 运行时创建）。所以必须先建 /var/tmp，/tmp 才可写。
$BB mkdir -p /var/tmp /run /dev/input /dev/virtio-ports 2>/dev/null
$BB chmod 1777 /var/tmp 2>/dev/null
$BB mkdir -p /tmp 2>/dev/null

# ---------------------------------------------------------------- 端口发现
# 不硬编码 vport0p0 / vport0p1 —— QEMU 给哪个端口分配哪个索引并不保证。
# 正确做法是读 /sys/class/virtio-ports/*/name，那是 QEMU 里 name= 的值。
echo "[guest] --- virtio-serial 端口发现 ---"
VPORTS_DIR=/sys/class/virtio-ports
FB_PORT=""
TOUCH_PORT=""

for _try in 1 2 3 4 5 6 7 8 9 10; do
    for d in $VPORTS_DIR/vport*; do
        [ -e "$d" ] || continue
        pname=$($BB cat "$d/name" 2>/dev/null)
        dev=/dev/$($BB basename "$d")
        [ -c "$dev" ] || continue
        case "$pname" in
            org.kindle.fb)    FB_PORT=$dev ;;
            org.kindle.touch) TOUCH_PORT=$dev ;;
        esac
    done
    [ -n "$FB_PORT" ] && [ -n "$TOUCH_PORT" ] && break
    $BB sleep 1
done

# 打印实际看到的东西，方便出问题时定位
echo "[guest] /sys/class/virtio-ports 内容："
for d in $VPORTS_DIR/vport*; do
    [ -e "$d" ] || continue
    echo "[guest]   $($BB basename $d)  name=$($BB cat $d/name 2>/dev/null)"
done
echo "[guest] /dev/vport* ：$($BB ls /dev/vport* 2>/dev/null | $BB tr '\n' ' ')"

if [ -n "$FB_PORT" ]; then
    $BB ln -sfn "$FB_PORT" /dev/virtio-ports/org.kindle.fb
    echo "[guest] fb 端口    : $FB_PORT -> /dev/virtio-ports/org.kindle.fb"
else
    echo "[guest] fb 端口    : 未找到！"
fi
if [ -n "$TOUCH_PORT" ]; then
    $BB ln -sfn "$TOUCH_PORT" /dev/virtio-ports/org.kindle.touch
    echo "[guest] 触摸端口   : $TOUCH_PORT -> /dev/virtio-ports/org.kindle.touch"
else
    echo "[guest] 触摸端口   : 未找到！"
fi

echo "[guest] 面板 ${PANEL_W}x${PANEL_H}"

# ---------------------------------------------------------------- 网络
# 只有需要走网络的模式才配网（remote = 跑应用取图；net = 纯网络自检）。
# 其余模式跳过，省得每次启动都白等一轮。

# 先把模式读出来（后面几处都要用）
MODE=$($BB cat /proc/cmdline 2>/dev/null | $BB tr ' ' '\n' | $BB grep '^kdev.mode=' | $BB cut -d= -f2)
[ -z "$MODE" ] && MODE=demo

case "$MODE" in remote|net|diag) WANT_NET=1 ;; *) WANT_NET=0 ;; esac

if [ "$WANT_NET" = 1 ]; then
    echo "[guest] --- 配置网络（QEMU user/slirp）---"
    # virtio_net 已经在 initramfs 阶段加载过了 —— 这里不能再 modprobe：
    # 现在已经在 Kindle 的根文件系统里，/lib/modules 只有 4.9.77-lab126，
    # 找不到 Debian 6.12 的模块。所以这里只是等设备出现。
    for _t in 1 2 3 4 5 6 7 8 9 10; do
        [ -e /sys/class/net/eth0 ] && break
        $BB sleep 1
    done

    if [ ! -e /sys/class/net/eth0 ]; then
        echo "[guest] 没有 eth0 —— virtio_net 没加载成功，网络不可用"
    else
        $BB ip link set eth0 up 2>/dev/null
        # slirp 的固定拓扑：guest 10.0.2.15，网关/宿主 10.0.2.2
        $BB ip addr add 10.0.2.15/24 dev eth0 2>/dev/null
        $BB ip route add default via 10.0.2.2 2>/dev/null
        $BB mkdir -p /var/tmp /etc
        echo "nameserver 10.0.2.3" > /etc/resolv.conf
        echo "[guest] eth0: $($BB ip -4 addr show eth0 2>/dev/null | $BB grep inet | $BB head -1)"

        SRV=$($BB cat /proc/cmdline 2>/dev/null | $BB tr ' ' '\n' | $BB grep '^kdev.server=' | $BB cut -d= -f2)
        [ -z "$SRV" ] && SRV=http://10.0.2.2:8000
        export MANGA_SERVER=$SRV
        echo "[guest] 服务端 $MANGA_SERVER"
        # 探测根路径：jm_kindle_server.py 和测试用的 mock 服务都在 "/" 上有内容
        if $BB wget -q -T 8 -O /dev/null "$MANGA_SERVER/" 2>/dev/null; then
            echo "[guest] 服务端可达"
        else
            echo "[guest] 服务端不可达（应用会显示错误，但界面仍会渲染）"
        fi
    fi
fi

# ---- 纯网络自检模式：验完就落到 shell，不启动应用 ----
if [ "$MODE" = net ]; then
    echo ""
    echo "[guest] ===== 网络自检 ====="
    echo "[guest] 路由表："
    $BB ip route 2>&1 | $BB sed 's/^/[guest]   /'
    echo "[guest] 接口："
    $BB ip -4 addr show eth0 2>&1 | $BB grep -E 'inet|state' | $BB sed 's/^/[guest]   /'

    PROBE=$MANGA_SERVER
    echo "[guest] 探测 $PROBE/"
    if $BB wget -T 10 -O /tmp/probe.out "$PROBE/" 2>&1 | $BB sed 's/^/[guest]   /'; then
        echo "[guest] 拿到 $( $BB wc -c < /tmp/probe.out 2>/dev/null ) 字节"
        echo "[guest] 内容：$( $BB head -c 200 /tmp/probe.out 2>/dev/null )"
    fi
    # 取一个「要去上游取数」的接口。这类接口服务端要花 1 秒以上才回应，
    # 用来把「网络层对慢响应的处理」和「客户端代码」分开验证。
    echo "[guest] --- 取本子详情（这条服务端要去上游，响应慢）---"
    if $BB wget -T 60 -O /tmp/probe.json "$PROBE/api/album/228415" 2>&1 | $BB sed 's/^/[guest]   /'; then
        echo "[guest] album 返回 $( $BB wc -c < /tmp/probe.json 2>/dev/null ) 字节"
        echo "[guest] 前 120 字符：$( $BB head -c 120 /tmp/probe.json 2>/dev/null )"
    else
        echo "[guest] album 取失败"
    fi

    echo "[guest] --- 再取一张图试试 ---"
    if $BB wget -T 10 -O /tmp/probe.jpg "$PROBE/img/0/0" 2>&1 | $BB sed 's/^/[guest]   /'; then
        SZ=$($BB wc -c < /tmp/probe.jpg 2>/dev/null)
        echo "[guest] 图片 $SZ 字节"
        # JPEG 魔数 FF D8，用来确认拿到的确实是图片而不是错误页。
        # 用 1/1 格式按字节顺序输出；若用默认的 16 位格式，小端会把 FF D8 显示成 d8ff。
        HEAD=$($BB hexdump -n 2 -e '1/1 "%02x"' /tmp/probe.jpg 2>/dev/null)
        echo "[guest] 文件头 $HEAD （JPEG 应为 ffd8）"
    fi
    echo "[guest] ===== 自检结束 ====="
    echo ""
    exec /bin/sh
fi

# ---- 调试模式：只给 shell ----
if $BB grep -q 'kdev.norun=1' /proc/cmdline 2>/dev/null; then
    echo "[guest] shell 模式（kdev.norun=1），不自动启动应用"
    echo "[guest] /dev 下的输入设备：$($BB ls /dev/input/ 2>/dev/null | $BB tr '\n' ' ')"
    exec /bin/sh
fi

# ---------------------------------------------------------------- 启动应用
cd /mnt/us/emu || exec /bin/sh

export FB_W=$PANEL_W
export FB_H=$PANEL_H
export FB_DEV=/dev/fb0
export TOUCH_W=$PANEL_W
export TOUCH_H=$PANEL_H
# 打开调试输出：出问题时能看到 shim 到底卡在哪一步
export FB_DEBUG=1
export TOUCH_DEBUG=1
export EMULATOR=1

# 端口路径显式传给 shim，省得它自己去猜
[ -n "$FB_PORT" ]    && export FB_VPORT="$FB_PORT"
[ -n "$TOUCH_PORT" ] && export TOUCH_VPORT="$TOUCH_PORT"

# lfs-compat 必须排在最前面：它把 Rust 用的 open64/mmap64 转发到普通符号，
# 后面的 fbshim/touchshim 才拦得到。
export LD_PRELOAD=/mnt/us/emu/liblfscompat.so:/mnt/us/emu/libfbshim.armhf.so:/mnt/us/emu/libtouchshim.armhf.so

echo "[guest] LD_PRELOAD=$LD_PRELOAD"
echo "[guest] FB_VPORT=$FB_VPORT TOUCH_VPORT=$TOUCH_VPORT"

# 诊断开关：kdev.notimeout=1 时让客户端不给请求挂超时。
# 用来判断「读超时」是不是 ureq 的超时机制本身造成的。
if $BB grep -q 'kdev.notimeout=1' /proc/cmdline 2>/dev/null; then
    export MANGA_NO_TIMEOUT=1
    echo "[guest] MANGA_NO_TIMEOUT=1（已关闭请求超时，诊断用）"
fi

# 日志级别：kdev.log=debug 会把它放开到 debug。
#
# 要这个开关是因为「每 N 次刷新做一次全屏刷新」这类决策只打在 debug 上
# （默认间隔是 1，用 info 会把日志刷爆），而模拟器里想确认这条路径真的走到了。
# 真机上不需要它：launch.sh 里直接 export 就行。
KLOG=$($BB cat /proc/cmdline 2>/dev/null | $BB tr ' ' '\n' | $BB grep '^kdev.log=' | $BB cut -d= -f2)
if [ -n "$KLOG" ]; then
    export MANGA_LOG=$KLOG
    echo "[guest] MANGA_LOG=$KLOG"
fi

# 模式（MODE）在文件开头已经读出来了，这里直接用：
#   demo（默认）—— 本地生成测试图
#   dir         —— 直接跑应用，从 /mnt/us/emu/pages 读 JPEG
#   remote      —— 走 HTTP 取图（真机的生产路径）
#   kual        —— 走 KUAL 的 launch.sh，验证真机那条启动路径
#   net         —— 纯网络自检，不启动应用（已在上面处理）
case "$MODE" in
    diag)
        # 网络自检的 A/B 对照：同一个二进制、同一条网络，只差 LD_PRELOAD。
        #
        # --diag 不建窗口、不碰帧缓冲，所以不需要 shim 也能跑起来 ——
        # 正好用来回答「是不是 shim 把网络搞坏了」这个问题。
        echo ""
        echo "[guest] ================ A. 不带 shim（LD_PRELOAD 空）================"
        echo "[guest] --- 先确认这条网络本身是好的：wget 取 album ---"
        $BB wget -q -T 30 -O /tmp/a.json "$MANGA_SERVER/api/album/228415" 2>&1 \
            && echo "[guest] wget 拿到 $( $BB wc -c < /tmp/a.json ) 字节" \
            || echo "[guest] wget 失败"
        echo "[guest] --- 客户端 --diag（无 shim）---"
        ( unset LD_PRELOAD; ./kindle-manga --diag ) 2>&1 | $BB sed 's/^/[no-shim] /'

        echo ""
        echo "[guest] ================ B. 带 shim（和跑应用时一样）================"
        ./kindle-manga --diag 2>&1 | $BB sed 's/^/[shim] /'

        echo ""
        echo "[guest] ================ 对照结束 ================"
        exec /bin/sh
        ;;
    dir)
        echo "[guest] 模式 dir：页面 $($BB ls /mnt/us/emu/pages 2>/dev/null | $BB tr '\n' ' ')"
        echo "[guest] 启动 kindle-manga --dir /mnt/us/emu/pages"
        ./kindle-manga --dir /mnt/us/emu/pages
        RC=$?
        ;;
    kual)
        echo "[guest] 模式 kual：走 /mnt/us/extensions/kindle-manga/launch.sh"
        # launch.sh 自己不设 LD_PRELOAD（真机不需要），但 LD_PRELOAD 会沿进程树
        # 继承，所以在模拟器里从外面设好，子进程照样能拿到 shim。
        export MANGA_DIR=/mnt/us/emu/pages
        $BB sh /mnt/us/extensions/kindle-manga/launch.sh
        RC=$?
        echo "[guest] launch.sh 返回 $RC"
        echo "[guest] 它写下的日志："
        $BB tail -5 /mnt/us/kindle-manga-logs/*.log 2>/dev/null
        ;;
    remote)
        echo "[guest] 模式 remote：走 HTTP 取图（真机的生产路径）"
        echo "[guest] MANGA_SERVER=$MANGA_SERVER"
        ./kindle-manga
        RC=$?
        ;;
    *)
        echo "[guest] 模式 demo"
        echo "[guest] 启动 kindle-manga --demo"
        ./kindle-manga --demo
        RC=$?
        ;;
esac
echo ""
echo "[guest] === 应用退出，返回码 $RC ==="
echo "[guest] 落回 shell"
exec /bin/sh

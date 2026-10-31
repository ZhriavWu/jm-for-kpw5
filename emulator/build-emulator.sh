#!/bin/bash
# 构建 Kindle 模拟环境：可写 rootfs 副本 + 自建 initramfs
#
# 为什么不用 Debian 的 mkinitramfs：它在 x86 主机上为 ARM 内核打包时，会把
# 主机的 x86 busybox 塞进去，initrd 里的 /init 根本跑不起来（qemu-kindle 为此
# 专门跑 ARM 容器）。这里改用自建 initramfs —— 全部组件都取自 Kindle 固件本身，
# 不存在任何架构错配。
#
# 两个容易踩的坑，都在下面处理了：
#   1. Debian armmp 内核把 ext4 和 virtio_blk 编成了**模块**（不是内建），
#      所以内核自己挂不了根文件系统，initramfs 是必需的而非可选。
#   2. Kindle 的 busybox 被 Amazon 裁掉了 mount / modprobe / insmod（326 个
#      applet 里没有这三个），必须另外从 rootfs 拷 /bin/mount 和 /bin/kmod，
#      而这两个是动态链接的，得连带把它们的库依赖闭包一起带上。

set -euo pipefail

ROOT=${ROOT:-/opt/kindle-dev}
KVER=6.12.94+deb13-armmp
SRC_ROOTFS=$ROOT/fw/rootfs.img
KDEB=$ROOT/qemu/armmp
EMU=$ROOT/emulator
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
TC=$HOME/x-tools/arm-kindlehf-linux-gnueabihf/bin
OUT=$EMU/out
TARGET=armv7-unknown-linux-gnueabihf
READELF=$TC/arm-kindlehf-linux-gnueabihf-readelf

say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }
mkdir -p "$OUT"

# ---------------------------------------------------------------- 1. shim
say "1/5 编译 ARM shim"
# 源码一律从项目目录读（$PROJ/emulator 是唯一真源），产物写到 $EMU。
# 早先源码和产物混在 $EMU 里，改了一边忘了同步另一边，直接编译失败。
CC="$TC/arm-kindlehf-linux-gnueabihf-gcc"
CFLAGS="-O2 -fPIC -shared -Wall -Wno-unused-parameter -Wno-nonnull-compare"
mkdir -p "$EMU"
$CC $CFLAGS -o "$EMU/libfbshim.armhf.so"    "$PROJ/emulator/fbshim.c"    -ldl -pthread
$CC $CFLAGS -o "$EMU/libtouchshim.armhf.so" "$PROJ/emulator/touchshim.c" -ldl -pthread
# lfs-compat：32 位 glibc 上 Rust 走 open64/mmap64，不转发的话会绕过上面两个 shim
$CC $CFLAGS -o "$EMU/liblfscompat.so"       "$PROJ/emulator/lfs-compat.c" -ldl
echo "  fbshim    $(stat -c%s "$EMU/libfbshim.armhf.so") 字节"
echo "  touchshim $(stat -c%s "$EMU/libtouchshim.armhf.so") 字节"
echo "  lfscompat $(stat -c%s "$EMU/liblfscompat.so") 字节"

# ---------------------------------------------------------------- 2. 应用
say "2/5 编译应用（$TARGET，动态链接以便注入 shim）"
export PATH="$TC:$PATH"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/target/kindle-manga}
( cd "$PROJ" && cargo build --release --features kindle --target "$TARGET" )
APP=$CARGO_TARGET_DIR/$TARGET/release/kindle-manga
[ -f "$APP" ] || { echo "找不到产物 $APP"; exit 1; }
cp "$APP" "$OUT/kindle-manga"
echo "  $(du -h "$OUT/kindle-manga" | cut -f1)  $OUT/kindle-manga"

# ---------------------------------------------------------------- 3. rootfs
say "3/5 准备可写 rootfs 副本"
# 每次都从原始镜像重拷，避免上一次的残留污染（改坏了好重来）
cp -f "$SRC_ROOTFS" "$OUT/rootfs-emu.img"

# 扩一块出来。源镜像是照真机分区做的，只剩 25 MB 左右空闲，而我们在里面
# 放了两份应用二进制（/mnt/us/emu/ 和 KUAL 扩展目录），加起来二十多兆。
# 一旦放不下，症状**不是构建报错**，而是应用启动时挂掉：
#     failed to stage default font: No space left on device
# slint-backend-kindle 要在可写的 /tmp 里落一份字体，写不下就 panic。
# 加输入法词典那会儿就是这么被撑爆的。
#
# 镜像是裸 ext3（没有分区表），所以 truncate + resize2fs 直接能扩。
truncate -s +512M "$OUT/rootfs-emu.img"
e2fsck -f -y "$OUT/rootfs-emu.img" >/dev/null 2>&1 || true   # 顺带把 journal 回放掉
resize2fs "$OUT/rootfs-emu.img" >/dev/null
echo "  已扩容：$(stat -c%s "$OUT/rootfs-emu.img") 字节"

MNT=$EMU/mnt-rootfs
mkdir -p "$MNT"
mount -o loop "$OUT/rootfs-emu.img" "$MNT"
trap 'umount "$MNT" 2>/dev/null || true' EXIT
mkdir -p "$MNT/mnt/us/emu"
cp "$OUT/kindle-manga"          "$MNT/mnt/us/emu/"
cp "$EMU/libfbshim.armhf.so"    "$MNT/mnt/us/emu/"
cp "$EMU/libtouchshim.armhf.so" "$MNT/mnt/us/emu/"
cp "$EMU/liblfscompat.so"       "$MNT/mnt/us/emu/"
cp "$PROJ/emulator/guest-init.sh" "$MNT/mnt/us/emu/"
chmod +x "$MNT/mnt/us/emu/kindle-manga" "$MNT/mnt/us/emu/guest-init.sh"

# 测试用 JPEG：验证真机那条「服务端发 JPEG → 客户端解码」的路径。
# demo 模式走的是 PPM，绕过了 JPEG 解码器，所以单独准备一套。
mkdir -p "$MNT/mnt/us/emu/pages"
python3 "$PROJ/emulator/make-test-pages.py" "$MNT/mnt/us/emu/pages" 4 2>&1 | sed 's/^/  /'

# KUAL 扩展：照真实布局放一份，用来在模拟器里验证 launch.sh 本身。
# 注意这里放的是**动态链接**版本而不是真机那份静态版本 —— 静态二进制没法被
# LD_PRELOAD 注入 shim，在模拟器里起不来。这条路径测的是 launch.sh 的脚本
# 逻辑（路径、日志、暂停/恢复流程），不是二进制本身。
mkdir -p "$MNT/mnt/us/extensions/kindle-manga"
cp "$PROJ/kual/extensions/kindle-manga/config.xml"  "$MNT/mnt/us/extensions/kindle-manga/" 2>/dev/null || true
cp "$PROJ/kual/extensions/kindle-manga/menu.json"   "$MNT/mnt/us/extensions/kindle-manga/" 2>/dev/null || true
cp "$PROJ/kual/extensions/kindle-manga/launch.sh"   "$MNT/mnt/us/extensions/kindle-manga/"
cp "$PROJ/kual/extensions/kindle-manga/show-log.sh" "$MNT/mnt/us/extensions/kindle-manga/"
cp "$OUT/kindle-manga"                              "$MNT/mnt/us/extensions/kindle-manga/kindle-manga"
chmod +x "$MNT/mnt/us/extensions/kindle-manga/kindle-manga" \
         "$MNT/mnt/us/extensions/kindle-manga/launch.sh" \
         "$MNT/mnt/us/extensions/kindle-manga/show-log.sh"
echo "  KUAL 扩展已放入 /mnt/us/extensions/kindle-manga/"
echo "  已写入 /mnt/us/emu/："
ls -la "$MNT/mnt/us/emu/"
umount "$MNT"
trap - EXIT

# ---------------------------------------------------------------- 4. initramfs
say "4/5 构建 initramfs"
IRD=$EMU/initrd-tree
rm -rf "$IRD"
mkdir -p "$IRD"/{bin,sbin,lib,proc,sys,dev,newroot}
mkdir -p "$IRD/lib/modules/$KVER"

KMNT=$EMU/mnt-kindlerc
mkdir -p "$KMNT"
mountpoint -q "$KMNT" || mount -o loop,ro "$SRC_ROOTFS" "$KMNT"
trap 'umount "$KMNT" 2>/dev/null || true' EXIT

# --- busybox（Kindle 自带，ARM + 它自己的 glibc）---
cp "$KMNT/bin/busybox" "$IRD/bin/busybox"
# switch_root 在 Kindle 上是 busybox 的软链接，这里照做
ln -sf /bin/busybox "$IRD/sbin/switch_root"

# --- mount / kmod：busybox 里被裁掉了，必须单独拷 ---
cp "$KMNT/bin/mount"  "$IRD/bin/mount"
cp "$KMNT/bin/umount" "$IRD/bin/umount"
cp "$KMNT/bin/kmod"   "$IRD/bin/kmod"
ln -sf /bin/kmod "$IRD/sbin/modprobe"
ln -sf /bin/kmod "$IRD/sbin/insmod"
ln -sf /bin/kmod "$IRD/sbin/lsmod"

# --- 递归解析库依赖闭包 ---
# /bin/mount 和 /bin/kmod 都是动态链接的，只拷二进制会跑不起来。
copy_lib_closure() {
  local queue=("$@") seen="" cur bin lib tgt d
  while [ ${#queue[@]} -gt 0 ]; do
    cur="${queue[0]}"; queue=("${queue[@]:1}")
    case " $seen " in *" $cur "*) continue ;; esac
    seen="$seen $cur"
    bin="$KMNT$cur"
    [ -e "$bin" ] || continue
    # 跟随软链接（kmod 的 modprobe 就是软链）
    while [ -L "$bin" ]; do
      tgt=$(readlink "$bin")
      case "$tgt" in
        /*) bin="$KMNT$tgt" ;;
        *)  bin="$(dirname "$bin")/$tgt" ;;
      esac
    done
    for lib in $("$READELF" -d "$bin" 2>/dev/null | awk '/NEEDED/{gsub(/[\[\]]/,"",$NF); print $NF}'); do
      for d in lib usr/lib; do
        if [ -e "$KMNT/$d/$lib" ]; then
          cp -Lf "$KMNT/$d/$lib" "$IRD/lib/" 2>/dev/null || true
          queue+=("/$d/$lib")
          break
        fi
      done
    done
  done
}
copy_lib_closure /bin/busybox /bin/mount /bin/umount /bin/kmod
echo "  initramfs 内库文件："
ls "$IRD/lib/" | grep -v modules | tr '\n' ' '; echo

# --- 挂载根文件系统所必需的模块 ---
# Debian 把模块压成 .ko.xz，所以按 glob 找，直接拼 .ko 找不到。
#
# 两个不能漏的：
#   * virtio_mmio —— vexpress 没有 PCI virtio，QEMU 的 virtio-blk-device 走 MMIO
#     传输，而 CONFIG_VIRTIO_MMIO=m。少了它 virtio_blk 能加载但绑不到设备，
#     /dev/vda 根本不出现。
#   * crc32c_generic —— ext4 的元数据校验要用 crc32c，走的是 crypto API 按名查找，
#     不是符号依赖，所以 modprobe ext4 不会自动带上它。缺了会报
#     "EXT4-fs (vda): Cannot load crc32c driver" 然后挂载失败。
KSRC=$KDEB/usr/lib/modules/$KVER
for m in virtio_mmio virtio_blk virtio_net net_failover failover ext4 jbd2 mbcache crc16 crc32c_generic; do
  src=$(find "$KSRC" -name "$m.ko*" 2>/dev/null | head -1)
  if [ -z "$src" ]; then echo "  [警告] 找不到模块 $m"; continue; fi
  rel=${src#"$KSRC"/}
  mkdir -p "$IRD/lib/modules/$KVER/$(dirname "$rel")"
  case "$src" in
    *.xz) xz -dc "$src" > "$IRD/lib/modules/$KVER/${rel%.xz}" ;;
    *)    cp "$src" "$IRD/lib/modules/$KVER/$rel" ;;
  esac
done
# depmod -b 的参数是「包含 lib/modules 的目录」，不是模块目录本身
depmod -b "$IRD" "$KVER" 2>&1 | grep -viE 'modules\.(order|builtin)' || true
if [ -s "$IRD/lib/modules/$KVER/modules.dep" ]; then
  echo "  modules.dep 已生成："
  sed 's/^/    /' "$IRD/lib/modules/$KVER/modules.dep"
else
  echo "  [警告] modules.dep 生成失败"
fi

# --- init ---
# 注意：Kindle 的 busybox 里**没有 mount applet**（Amazon 裁掉了），所以挂载必须
# 调用独立二进制 /bin/mount。另外 initramfs 里没有 /etc/mtab，得加 -n 让它别去写，
# 否则 mount 会因为建不了锁文件而返回失败。
cat > "$IRD/init" <<'INIT'
#!/bin/busybox sh
# Kindle 模拟环境的 initramfs init。
# 职责：挂上伪文件系统 → 加载 virtio_blk/ext4 → 挂载 Kindle 根分区 → 切过去。
export PATH=/bin:/sbin
BB=/bin/busybox
MNT=/bin/mount

$BB mkdir -p /etc /proc /sys /dev /newroot
$BB ln -sfn /proc/mounts /etc/mtab

$MNT -n -t proc     proc /proc
$MNT -n -t sysfs    sys  /sys
$MNT -n -t devtmpfs dev  /dev 2>/dev/null || $MNT -n -t tmpfs dev /dev

# 网络模块也必须在这里加载，不能放到 switch_root 之后：switch_root 之后就进了
# Kindle 自己的根文件系统，那里的 /lib/modules 只有 4.9.77-lab126，找不到
# Debian 6.12 的模块。块设备那几个能用，正是因为它们是在这一步（initramfs 内）加载的。
for m in virtio_mmio virtio_blk virtio_net net_failover failover crc32c_generic ext4; do
    if /sbin/modprobe "$m" 2>/dev/null; then
        echo "[init] 已加载 $m"
    else
        echo "[init] 加载 $m 失败"
    fi
done

# eth0 一般随 virtio_net 立刻出现，等一下更稳
if [ -e /sys/class/net/eth0 ]; then
    echo "[init] eth0 已就绪"
else
    echo "[init] 警告：eth0 未出现，网络可能不可用"
fi

# virtio-blk 的盘符有时是 /dev/vda，有时是 /dev/vda1，两个都试
ROOTDEV=
for d in /dev/vda /dev/vda1; do
    if [ -b "$d" ] && $MNT -n -t ext4 "$d" /newroot 2>/dev/null; then
        ROOTDEV=$d
        break
    fi
done

if [ -n "$ROOTDEV" ]; then
    echo "[init] Kindle 根文件系统已挂载（$ROOTDEV）"
    $BB mkdir -p /newroot/proc /newroot/sys /newroot/dev
    # Kindle 的 /tmp 是指向 /var/tmp 的软链接，而 /var/tmp 在镜像里并不存在
    # （真机上由 Kindle 自己的 init 在运行时创建）。不建的话切根之后任何往
    # /tmp 写文件的操作都会失败。
    $BB mkdir -p /newroot/var/tmp
    $BB chmod 1777 /newroot/var/tmp
    $MNT -n --move /proc /newroot/proc 2>/dev/null || true
    $MNT -n --move /sys  /newroot/sys  2>/dev/null || true
    $MNT -n --move /dev  /newroot/dev  2>/dev/null || true
    echo "[init] 切换到 Kindle 环境"
    # 有 guest-init.sh 就走它（会自动带 shim 跑应用），否则给个裸 shell
    if [ -f /newroot/mnt/us/emu/guest-init.sh ]; then
        exec /sbin/switch_root /newroot /bin/busybox sh /mnt/us/emu/guest-init.sh
    fi
    exec /sbin/switch_root /newroot /bin/sh
else
    echo "[init] 挂载失败 —— 进入救援 shell"
    $BB ls -la /dev/vd* 2>/dev/null
    exec $BB sh
fi
INIT
chmod +x "$IRD/init"

( cd "$IRD" && find . | cpio -o -H newc --quiet | gzip -9 ) > "$OUT/initrd.img"
echo "  $(du -h "$OUT/initrd.img" | cut -f1)  $OUT/initrd.img"

umount "$KMNT"
trap - EXIT

# ---------------------------------------------------------------- 5. 汇总
say "5/5 完成"
cat <<EOF
产物目录：$OUT
  rootfs-emu.img   可写 rootfs（已含 /mnt/us/emu/ 下的应用与 shim）
  initrd.img       自建 initramfs
  kindle-manga     应用二进制

内核：$KDEB/boot/vmlinuz-$KVER
设备树：$KDEB/usr/lib/linux-image-$KVER/vexpress-v2p-ca9.dtb

下一步：emulator/run-emulator.sh
EOF

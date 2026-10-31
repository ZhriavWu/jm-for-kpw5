#!/bin/bash
# 打包 KUAL 扩展：编译真机二进制并组装成可以直接拖进 Kindle 的目录。
#
# 产出：
#   kual/extensions/kindle-manga/       ← 整个文件夹拖到 Kindle 根目录的 extensions/ 下
#   kual/kindle-manga-<版本>.zip        ← 同上内容的压缩包，便于传输
#
# 注意：只把二进制本身拷进扩展目录。不要把 target/ 拖进去 —— 那是好几个 GB。

set -euo pipefail

ROOT=${ROOT:-/opt/kindle-dev}
PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
TARGET=armv7-unknown-linux-musleabihf
EXT=$PROJ/kual/extensions/kindle-manga

say() { printf '\n\033[1m== %s ==\033[0m\n' "$*"; }

# ---------------------------------------------------------------- 1. 编译
say "1/5 编译真机二进制（$TARGET，静态 musl）"
. "$HOME/.cargo/env" 2>/dev/null || true
export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
export CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/target/kindle-manga}

( cd "$PROJ" && cargo zigbuild --release --features kindle --target "$TARGET" )

BIN=$CARGO_TARGET_DIR/$TARGET/release/kindle-manga
[ -f "$BIN" ] || { echo "找不到产物 $BIN"; exit 1; }

# 确认是静态链接 —— 动态链接的版本在没越狱/库不全的机器上会起不来
if ! file "$BIN" | grep -q 'statically linked'; then
  echo "警告：产物不是静态链接，真机上可能起不来"
  file "$BIN"
fi
echo "  $(du -h "$BIN" | cut -f1)  $BIN"

# ---------------------------------------------------------------- 1.5 内置服务端
#
# 服务端也用 Go 交叉编译成 ARM 静态二进制，和客户端打进**同一个 KUAL 扩展**。
# 放在扩展目录而不是别处，是因为 KUAL 的扩展是「整个文件夹拖进去」的单位，
# 分两处放用户很容易只拷了一半。
#
# ⚠️ Go 模块里写的是 `go 1.26.0`；WSL 自带的 Go 1.22 会尝试自动下载工具链。
#    走 goproxy.cn 就能下（默认的 proxy.golang.org 在国内会超时）。
say "1.5/5 编译内置服务端（Go，linux/arm，静态）"
SRV_BIN=$PROJ/kual/extensions/kindle-manga/kindle-manga-server
(
  cd "$PROJ/server"
  GOPROXY=${GOPROXY:-https://goproxy.cn,direct} \
  CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=7 \
    go build -trimpath -ldflags "-s -w" -o "$SRV_BIN" .
)
[ -f "$SRV_BIN" ] || { echo "找不到服务端产物 $SRV_BIN"; exit 1; }
if ! file "$SRV_BIN" | grep -q 'ARM'; then
  echo "警告：服务端产物不是 ARM"; file "$SRV_BIN"
fi
echo "  $(du -h "$SRV_BIN" | cut -f1)  $SRV_BIN"

# ---------------------------------------------------------------- 2. 组装
say "2/5 组装扩展目录"
mkdir -p "$EXT"
cp -f "$BIN" "$EXT/kindle-manga"
chmod 755 "$EXT/kindle-manga" "$EXT/kindle-manga-server" "$EXT/launch.sh" "$EXT/show-log.sh" 2>/dev/null || true

# server.txt 是**运行时**产物（用户在界面上填了地址才会生成），不该打进包里。
# 原因：它会盖过 launch.sh 里的 MANGA_SERVER。要是哪次本地调试写进去一个地址，
# 打包带走之后，用户改 launch.sh 就不生效了 —— 排查起来很费劲。
# 默认地址统一由 launch.sh 的 MANGA_SERVER 提供，这里只负责别让它混进来。
if [ -e "$EXT/server.txt" ]; then
  echo "  移除 server.txt（运行时文件，不随包分发）"
  rm -f "$EXT/server.txt"
fi

# 字体不需要单独放：真机构建里已经 include_bytes! 编进二进制了
echo "  扩展目录内容："
ls -la "$EXT" | sed 's/^/    /'

# ---------------------------------------------------------------- 3. 校验
say "3/5 校验"
fail=0
for f in config.xml menu.json launch.sh show-log.sh kindle-manga kindle-manga-server; do
  if [ -e "$EXT/$f" ]; then
    echo "  OK   $f"
  else
    echo "  缺失 $f"
    fail=1
  fi
done

# config.xml 里的 id 必须和目录名一致，否则 KUAL 加载不到
if grep -q '<id>kindle-manga</id>' "$EXT/config.xml"; then
  echo "  OK   config.xml 的 id 与目录名一致"
else
  echo "  警告 config.xml 的 <id> 与目录名 kindle-manga 不一致，KUAL 可能找不到菜单"
  fail=1
fi

# 二进制架构检查（客户端和服务端都要）
for b in kindle-manga kindle-manga-server; do
  if file "$EXT/$b" | grep -q 'ARM'; then
    echo "  OK   $b 是 ARM"
  else
    echo "  错误 $b 不是 ARM 架构"
    fail=1
  fi
done

# launch.sh 必须真的会起服务端 —— 少了它整套东西退回「必须开电脑」
if grep -q 'kindle-manga-server' "$EXT/launch.sh"; then
  echo "  OK   launch.sh 会启动内置服务端"
else
  echo "  错误 launch.sh 里找不到 kindle-manga-server"
  fail=1
fi

[ "$fail" = 0 ] || { echo "校验未通过"; exit 1; }

# ---------------------------------------------------------------- 4. 压缩
say "4/5 生成压缩包"
VER=$(grep -oP '(?<=<version>)[^<]+' "$EXT/config.xml" | head -1)
ZIP=$PROJ/kual/kindle-manga-$VER.zip
rm -f "$ZIP"
( cd "$PROJ/kual" && zip -qr "$ZIP" extensions )
echo "  $(du -h "$ZIP" | cut -f1)  $ZIP"

say "完成"
cat <<EOF
部署方式（二选一）：

A. USB 拖拽（不需要 SSH）
   1. USB 连接 Kindle，等它出现在"此电脑"里
   2. 打开 Kindle 根目录，找到（没有就新建）extensions 文件夹
   3. 把整个 kindle-manga 文件夹拖进去，最终路径：
        Kindle/extensions/kindle-manga/kindle-manga
   4. 弹出设备，在 Kindle 上打开 KUAL，应能看到"启动漫画阅读器"

B. SSH（开发调试用，需要先装 USBNetLite）
   scp -r $EXT root@<Kindle的IP>:/mnt/us/extensions/
   # Kindle 的 IP 看 设置 → 设备选项 → 设备信息，或路由器后台

服务端已经内置（kindle-manga-server，只监听 127.0.0.1）：
   launch.sh 会在启动应用前把它拉起来，退出时关掉。**不再需要电脑常开。**
   * 想改用电脑上的 Python 服务端：在 Kindle 里「设置 → 服务器地址」填电脑 IP
     （写进 server.txt，优先级高于内置）
   * 想换回内置：把那个地址清空即可
   * 内置服务端的图片缓存放在 /mnt/us/kindle-manga-cache，日志在同目录
     server-<时间>.log

注意：Kindle 用户分区是 FAT32，没有 Unix 权限概念，文件默认可执行，
不需要也不能靠 chmod 来保证。
EOF

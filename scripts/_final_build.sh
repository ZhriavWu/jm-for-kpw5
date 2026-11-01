#!/bin/bash
# 最终验收 + 编译打包。
set -u
cd "$(dirname "$0")/.." || exit 1
PROJ=$(cd "$(dirname "$0")/.." && pwd)
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga

echo "################ 0) 停掉测试残留 ################"
pkill -f '_slow_proxy\.py' 2>/dev/null && echo "  已停慢代理" || echo "  没有代理在跑"
pkill -x kindle-manga 2>/dev/null && echo "  已停旧实例" || echo "  没有旧实例"
rm -f /root/target/kindle-manga/debug/server.txt

echo
echo "################ 1) 强制重编 + 确认真的编了 ################"
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^(error|warning)' /tmp/fb.log | head -10 || true

echo
echo "################ 2) 全量测试（--workspace）################"
bash "$(dirname "$0")/_test.sh" 2>&1 | tail -12

echo
echo "################ 3) 规格检查（15 条）################"
bash "$PROJ/check-ui-spec.sh" 2>&1 | tail -4

echo
echo "################ 4) 字体覆盖 ################"
bash "$PROJ/check-font-coverage.sh" 2>&1 | tail -3

echo
echo "################ 5) 真机包：ARM musl 静态 + KUAL 扩展 ################"
bash kual/package-kual.sh 2>&1 | tail -8

echo
echo "################ 6) 产物 ################"
ls -la kual/extensions/kindle-manga/kindle-manga kual/*.zip
file kual/extensions/kindle-manga/kindle-manga

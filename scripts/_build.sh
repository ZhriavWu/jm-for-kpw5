#!/bin/bash
# 一次性：编译并只看错误行。避开 PowerShell 对 ^ 和中文的破坏。看完可删。
# 用法：bash "$(dirname "$0")/_build.sh" [desktop|arm|test]
set -u
MODE="${1:-desktop}"
. "$HOME/.cargo/env"
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=/root/target/kindle-manga

case "$MODE" in
  desktop) cargo build --features desktop ;;
  arm)     cargo build --release --features kindle --target armv7-unknown-linux-gnueabihf ;;
  test)    cargo test --workspace --target x86_64-unknown-linux-gnu ;;
  *)       echo "unknown mode: $MODE"; exit 2 ;;
esac > /tmp/build.log 2>&1
RC=$?

echo "exit code: $RC"
echo "--- 错误 / 警告 ---"
grep -n "error\|warning" /tmp/build.log | head -40
echo "--- 末尾 ---"
tail -6 /tmp/build.log
exit $RC

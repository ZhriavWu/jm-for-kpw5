#!/bin/bash
# 只编译 + 看错误/警告，快。
set -u
. "$HOME/.cargo/env"
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=/root/target/kindle-manga

cargo build --features desktop > /tmp/b.log 2>&1
RC=$?
echo "exit=$RC  Compiling=$(grep -c 'Compiling' /tmp/b.log)"
echo "--- 错误/警告 ---"
grep -E '^(error|warning)' /tmp/b.log | head -20
echo "--- 错误详情 ---"
grep -n -A14 '^error' /tmp/b.log | head -60
exit $RC

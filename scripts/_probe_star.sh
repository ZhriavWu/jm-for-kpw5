#!/bin/bash
set -u
. "$HOME/.cargo/env"
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=/root/target/kindle-manga

cargo test --workspace --target x86_64-unknown-linux-gnu > /tmp/t.log 2>&1
echo "exit=$?"
echo "===== 编译错误 ====="
grep -n -A16 '^error' /tmp/t.log | head -70
echo
echo "===== 失败明细 ====="
grep -n -B2 -A20 'panicked\|failures:' /tmp/t.log | head -50
echo
echo "===== 结果 ====="
grep -E 'test result|running [0-9]+ tests' /tmp/t.log
echo
echo "===== 警告 ====="
grep -cE '^warning' /tmp/t.log

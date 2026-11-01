#!/bin/bash
# 最终验收：强制重编 + 三个检查。
set -u
cd "$(dirname "$0")/.." || exit 1
PROJ=$(cd "$(dirname "$0")/.." && pwd)
. "$HOME/.cargo/env" 2>/dev/null || true
export CARGO_TARGET_DIR=/root/target/kindle-manga

echo "########## 强制重编（确认真的编了）##########"
touch ui/*.slint build.rs
cargo build --features desktop > /tmp/fb.log 2>&1
echo "exit=$?  Compiling=$(grep -c 'Compiling' /tmp/fb.log)"
grep -E '^(error|warning)' /tmp/fb.log | head -20 || true

echo
echo "########## check-ui-spec.sh ##########"
bash "$PROJ/check-ui-spec.sh" 2>&1 | tail -3

echo
echo "########## check-font-coverage.sh ##########"
bash "$PROJ/check-font-coverage.sh" 2>&1 | tail -2

echo
echo "########## 单元测试 ##########"
bash "$(dirname "$0")/_test.sh" 2>&1 | tail -11

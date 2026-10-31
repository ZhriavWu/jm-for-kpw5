#!/bin/bash
set -u
. "$HOME/.cargo/env"
cd "$(dirname "$0")" || exit 1
export CARGO_TARGET_DIR=/root/target/kindle-manga
cargo test --workspace --target x86_64-unknown-linux-gnu > /tmp/t.log 2>&1
echo "exit=$?"
grep -E '^(running|test result:)' /tmp/t.log
echo "--- 警告数（应为 0）---"
grep -c '^warning' /tmp/t.log || true
echo "--- 失败明细（应为空）---"
grep -E 'FAILED|failures:' /tmp/t.log || echo '（无）'

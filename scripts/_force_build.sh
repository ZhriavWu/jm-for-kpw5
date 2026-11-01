#!/bin/bash
# 一次性：强制重编（把 .slint 的 mtime 推一下，确保 build.rs 真的重跑）。
#
# 为什么要这个：有一次改完 ui/*.slint 再 build，cargo 只用了 0.41 秒、
# **一行 Compiling 都没有** —— 等于什么都没编，而看输出还以为成功了。
# 之后拿旧二进制验了半天，白费工夫。
set -u
. "$HOME/.cargo/env"
cd "$(dirname "$0")/.." || exit 1
export CARGO_TARGET_DIR=/root/target/kindle-manga

touch ui/*.slint build.rs

cargo build --release --features desktop > /tmp/fb.log 2>&1
RC=$?
echo "exit=$RC"
echo "--- 有没有真的编译（不该是 0）---"
grep -c 'Compiling' /tmp/fb.log || true
grep -m1 'Compiling' /tmp/fb.log || true
echo "--- 错误/警告 ---"
grep -E '^(error|warning)' /tmp/fb.log | head -20 || echo "（无）"
tail -3 /tmp/fb.log
echo "--- 产物 ---"
stat -c '%y  %s 字节' /root/target/kindle-manga/release/kindle-manga
exit $RC

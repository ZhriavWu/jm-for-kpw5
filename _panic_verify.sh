#!/bin/bash
# 验证「长条漫切屏不越界」这条测试：
#   正向 —— 修好的代码应该通过
#   负向 —— 把夹取去掉，测试必须失败（否则就是假绿灯）
set -u
cd "$(dirname "$0")" || exit 1
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR=/root/target/kindle-manga

# ⚠️ 这个项目**没有 lib 目标**，测试都在 bin 里 —— 必须写 `--bin kindle-manga`，
#    写成 `--lib` 会直接报 "no library targets found"（退出码 101，看着像测试失败）。
run_test() {
    cargo test --bin kindle-manga --target x86_64-unknown-linux-gnu 长条漫切屏不越界 -- --nocapture \
        > /tmp/pt.log 2>&1
    local rc=$?
    grep -E 'test result|panicked at|out of range|^error' /tmp/pt.log | head -5
    return $rc
}

echo "########## 正向：带夹取的代码 ##########"
run_test
echo "退出码 = $?（应为 0）"

echo
echo "########## 负向：去掉夹取，测试必须失败 ##########"
cp src/render.rs /tmp/render.bak
python3 - <<'PY'
p = "src/render.rs"
s = open(p, encoding="utf-8").read()
old = """    let dst_w = (((w as f32) * scale).round() as usize).clamp(1, VIEW_W);
    let dst_h = (((take as f32) * scale).round() as usize).clamp(1, VIEW_H);"""
new = """    let dst_w = ((w as f32) * scale).round().max(1.0) as usize;
    let dst_h = ((take as f32) * scale).round().max(1.0) as usize;"""
if old not in s:
    print("！！没找到要还原的片段"); raise SystemExit(1)
open(p, "w", encoding="utf-8").write(s.replace(old, new, 1))
print("   已去掉 clamp，还原成崩溃前的写法")
PY
run_test
echo "退出码 = $?（应为非 0）"

echo
echo "########## 还原 ##########"
cp /tmp/render.bak src/render.rs
run_test
echo "退出码 = $?（应为 0）"

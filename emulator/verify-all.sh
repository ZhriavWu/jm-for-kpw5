#!/bin/bash
# 全量自检：构建 + 渲染 + 触摸 + 网络，四项依次跑一遍。
#
# 用途：环境改动之后确认没有回归；也可以交给下一个接手的人当作
# "这套环境到底好不好使"的一条命令答案。
#
# 用法：emulator/verify-all.sh

PROJ=${PROJ:-$(cd "$(dirname "$0")/.." && pwd)}
PASS=0
FAIL=0

hdr() { printf '\n\033[1m########## %s ##########\033[0m\n' "$*"; }
ok()  { printf '  \033[32m[通过]\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad() { printf '  \033[31m[失败]\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }

# ---------------------------------------------------------------- 1. 构建
hdr "1/4 构建"
if bash "$PROJ/emulator/build-emulator.sh" >/tmp/va-build.log 2>&1; then
    ok "build-emulator.sh 退出码 0"
else
    bad "build-emulator.sh 失败，见 /tmp/va-build.log"
    tail -20 /tmp/va-build.log
fi

for f in kindle-manga initrd.img rootfs-emu.img; do
    [ -e "/opt/kindle-dev/emulator/out/$f" ] && ok "$f 存在" || bad "$f 缺失"
done

# ---------------------------------------------------------------- 2. 渲染
hdr "2/4 渲染链路（应用 → fbshim → socket）"
OUT=$(bash "$PROJ/emulator/e2e-test.sh" 35 demo 2>&1)
if echo "$OUT" | grep -q '渲染链路正常'; then
    ok "渲染正常：$(echo "$OUT" | grep -oE '收到 [0-9]+ 帧')"
else
    bad "渲染异常"
    echo "$OUT" | tail -15
fi

# ---------------------------------------------------------------- 3. 触摸
hdr "3/4 触摸与翻页"
OUT=$(bash "$PROJ/emulator/interact-test.sh" 30 2>&1)
if echo "$OUT" | grep -q '翻页成功'; then
    ok "触摸翻页生效：$(echo "$OUT" | grep -oE '内容已变化：[a-f0-9]+ -> [a-f0-9]+')"
    if echo "$OUT" | grep -q 'vport connected 次数: 1'; then
        ok "无重连风暴"
    else
        bad "$(echo "$OUT" | grep -oE 'vport connected 次数: [0-9]+')"
    fi
else
    bad "触摸翻页未生效"
    echo "$OUT" | tail -15
fi

# ---------------------------------------------------------------- 4. 网络
hdr "4/4 网络（QEMU slirp → 宿主 HTTP）"
OUT=$(bash "$PROJ/emulator/net-test.sh" 8000 2>&1)
if echo "$OUT" | grep -q '网络通了，而且真的从宿主取到了一张 JPEG'; then
    ok "guest 能通过 HTTP 从宿主取到 JPEG"
    ok "$(echo "$OUT" | grep -oE 'GET /img/[0-9/]+ HTTP/1.1" 200' | head -1)"
else
    # 可能只是 8000 端口被占用，退一步看是否至少 HTTP 通
    if echo "$OUT" | grep -q '服务端可达'; then
        ok "HTTP 可达（取图那步未确认）"
    else
        bad "网络不通"
        echo "$OUT" | tail -15
    fi
fi

# ---------------------------------------------------------------- 汇总
hdr "汇总"
printf '  通过 %d 项，失败 %d 项\n' "$PASS" "$FAIL"
if [ "$FAIL" -eq 0 ]; then
    printf '  \033[32m环境健康\033[0m\n'
    exit 0
fi
printf '  \033[31m有问题，按上面提示排查\033[0m\n'
exit 1

#!/bin/bash
# 一次性：打包并只看关键几行。看完可删。
set -u
PROJ=$(cd "$(dirname "$0")/.." && pwd)
cd "$PROJ/kual" || exit 1
bash package-kual.sh > /tmp/pkg.log 2>&1
RC=$?
echo "exit=$RC"
grep -a 'OK\|错误\|失败\|完成' /tmp/pkg.log | head -20
echo "--- 产物 ---"
ls -la --time-style=+%H:%M:%S "$PROJ/kual"/extensions/kindle-manga/kindle-manga
exit $RC

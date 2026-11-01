#!/bin/bash
# 编译 ARM 服务端（一次性辅助脚本，可删）。
set -u
cd "$(dirname "$0")/../server" || exit 1
export GOPROXY=${GOPROXY:-https://goproxy.cn,direct}
export CGO_ENABLED=0
export GOOS=linux
export GOARCH=arm
export GOARM=7
go build -trimpath -ldflags '-s -w' -o /tmp/arm_server . 2>&1
echo "RC=$?"
ls -l /tmp/arm_server 2>&1
file /tmp/arm_server 2>&1

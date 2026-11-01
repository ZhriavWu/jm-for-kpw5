#!/bin/bash
# 检查播下去的 favorites.json 是不是合法 UTF-8 / 合法 JSON。
set -u
F=/root/target/kindle-manga/release/favorites.json
echo "--- 原始字节 ---"
xxd "$F" | head -6
echo "--- 当 UTF-8 解码 ---"
python3 - "$F" <<'PY'
import json, sys
raw = open(sys.argv[1], 'rb').read()
try:
    txt = raw.decode('utf-8')
    print("UTF-8 解码 OK")
except UnicodeDecodeError as e:
    print("UTF-8 解码失败:", e); raise SystemExit(1)
try:
    obj = json.loads(txt)
    print("JSON 合法，id =", [o['id'] for o in obj], "title =", [o['title'] for o in obj])
except Exception as e:
    print("JSON 非法:", e)
PY

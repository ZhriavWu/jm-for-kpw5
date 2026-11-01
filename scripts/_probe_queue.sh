#!/bin/bash
# 测服务端各接口耗时 —— 先知道「加载中」这个窗口有多长，才好复现排队。
set -u
GW=$(ip route show default | awk '{print $3}')
B="http://$GW:8001"

echo "=== /api/info ==="
curl -s -m 10 -o /dev/null -w '  HTTP %{http_code}  %{time_total}s\n' "$B/api/info" || echo "  失败"

echo
echo "=== /api/search（冷关键词，可能触发服务端域名探测）==="
for q in zzqcold1 naruto 原神; do
    printf '  q=%-10s ' "$q"
    curl -s -m 60 -o /tmp/s.json -w 'HTTP %{http_code}  %{time_total}s' \
        --get --data-urlencode "q=$q" --data "p=1" "$B/api/search" || echo -n "失败"
    echo "  大小 $(stat -c %s /tmp/s.json 2>/dev/null || echo 0)B"
done

echo
echo "=== /api/search 再来一次同样的词（测有没有缓存）==="
for q in zzqcold1 naruto; do
    printf '  q=%-10s ' "$q"
    curl -s -m 30 -o /dev/null -w 'HTTP %{http_code}  %{time_total}s\n' \
        --get --data-urlencode "q=$q" --data "p=1" "$B/api/search" || echo "失败"
done

echo
echo "=== /cover/<id>（封面，看看慢不慢）==="
for id in 350234 1475681; do
    printf '  id=%-9s ' "$id"
    curl -s -m 60 -o /tmp/c.bin -w 'HTTP %{http_code}  %{time_total}s' "$B/cover/$id" || echo -n "失败"
    echo "  大小 $(stat -c %s /tmp/c.bin 2>/dev/null || echo 0)B"
done

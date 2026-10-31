#!/bin/bash
# 看出错页右上角的退出按钮：位置对不对、点下去真的能退出吗。
#
# ⚠️ 要用 `nohup` 起 —— 直接 `&` 的话脚本一退出应用就跟着被杀
#    （它是这个 bash 会话的子进程），上一次就栽在这上面。
#
# 退出按钮（逻辑）：x = 1236-36-110 = 1090，y = 108，110×110
#   → 物理 x 545..600，y 54..109
# 标题（逻辑）：x 60..1000 → 物理 x 30..500 —— 和按钮**横向不重叠**，要量一下确认。
set -u

OUT=/root/target/kindle-manga/debug
BIN=$OUT/kindle-manga
DEAD=http://127.0.0.1:9
LOG=/tmp/err_quit.log

printf '%s' "$DEAD" > "$OUT/server.txt"
export DISPLAY=:0
export SLINT_SCALE_FACTOR=0.5
pkill -x kindle-manga 2>/dev/null && sleep 1
: > "$LOG"

nohup env -u MANGA_SERVER "$BIN" --server "$DEAD" > "$LOG" 2>&1 &
sleep 20

WID=$(xdotool search --name kindle-manga 2>/dev/null | head -1)
[ -n "$WID" ] || { echo "!! 没窗口"; tail -20 "$LOG"; exit 1; }
xdotool windowactivate "$WID" 2>/dev/null
sleep 1
echo "当前屏：$(grep -a '界面 ' "$LOG" | tail -1)"

xdotool mousemove --window "$WID" 5 300
sleep 1.5
import -window "$WID" /tmp/err2.png

echo
echo "===== 出错页顶部（物理 y 44..125）====="
python3 - <<'PY'
import subprocess
png = "/tmp/err2.png"
r = subprocess.run(["convert", png, "-crop", "618x82+0+44", "+repage",
                    "-colorspace", "Gray", "-depth", "8", "-resize", "103x82!", "pgm:-"],
                   capture_output=True)
d = r.stdout
st = [0]
def tok():
    p = st[0]
    while d[p:p+1].isspace(): p += 1
    s = p
    while not d[p:p+1].isspace(): p += 1
    st[0] = p
    return d[s:p]
assert tok() == b"P5"
W, H = int(tok()), int(tok()); tok(); st[0] += 1
pix = d[st[0]:st[0]+W*H]
print("（每字符 ≈ 6 物理px 宽）")
for yy in range(0, H, 2):
    y = 44 + yy
    line = "".join("#" if pix[yy*W+xx] < 128 else ("+" if pix[yy*W+xx] < 200 else ".") for xx in range(W))
    mark = ""
    if y in (54, 56): mark = "  ← 退出按钮顶(物理54)"
    if y in (108, 110): mark = "  ← 退出按钮底(物理109)"
    print("%4d %s%s" % (y, line, mark))
print()
print("  横向：退出按钮该在物理 545..600（右边那一块方框）")
PY

echo
echo "===== 点退出按钮（窗口内物理 572,81）====="
# ⚠️ 别用 `pgrep -c` 判断。
#    pgrep 在**没有匹配时既打印 "0" 又返回非零**，所以
#    `$(pgrep -c -x X || echo 0)` 会拼出两个 0 → 字符串比较失败 →
#    明明退出了却报「没退出」。上一次就栽在这上面（假失败）。
#    改用「数一下有没有匹配」。
count_app() { pgrep -x kindle-manga | wc -l; }

BEFORE=$(count_app)
echo "  点击前进程数: $BEFORE"
[ "$BEFORE" -gt 0 ] || { echo "  !! 点击前应用就没在跑，测试无效"; exit 1; }

xdotool mousemove --window "$WID" 572 81 click 1
sleep 3
AFTER=$(count_app)
echo "  点击后进程数: $AFTER"
if [ "$AFTER" -eq 0 ]; then
    echo "  ✅ 退出按钮生效 —— 应用已退出"
else
    echo "  ❌ 点了没退出"
    exit 1
fi

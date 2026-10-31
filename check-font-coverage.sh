#!/bin/bash
# 一次性：把 ui/*.slint 里**所有界面文字**的字符拿去和字体 cmap 比对，
# 找出画不出来的字。看完可删。
#
# 为什么必须机器查：字体缺字时 Slint 画的是**空白**（不是方框），
# 而且不报任何错。设计稿是 HTML（浏览器有回退字体），真机没有 ——
# 所以在浏览器里看着好好的字，到 Kindle 上可能整段消失。
set -u
cd "$(dirname "$0")" || exit 1

python3 - <<'PY'
import re, sys, pathlib

FONT = "fonts/WenQuanYiMicroHei-Subset.ttf"

# ---- 读字体的 cmap ----
try:
    from fontTools.ttLib import TTFont
    f = TTFont(FONT)
    cmap = set()
    for t in f['cmap'].tables:
        cmap |= set(t.cmap.keys())
    print(f"字体 {FONT}：{len(cmap)} 个码位")
except ImportError:
    print("没有 fontTools，改用原始 cmap 解析")
    sys.exit(2)

# ---- 抽出 ui/*.slint 里所有双引号字符串字面量 ----
used = {}
for p in sorted(pathlib.Path("ui").glob("*.slint")):
    for lineno, line in enumerate(p.read_text(encoding="utf-8").splitlines(), 1):
        code = line.split("//")[0]          # 去掉行尾注释
        for s in re.findall(r'"([^"\\]*(?:\\.[^"\\]*)*)"', code):
            for ch in s:
                if ch in "\n\r\t":
                    continue
                used.setdefault(ch, []).append(f"{p.name}:{lineno}")

# ---- 比对 ----
missing = {ch: locs for ch, locs in used.items() if ord(ch) not in cmap}
# 空白字符不算
missing = {c: l for c, l in missing.items() if not c.isspace()}

print(f"界面文字用到 {len(used)} 个不同字符")
print()
if not missing:
    print("✅ 全部字符字体都画得出来")
else:
    print(f"❌ 有 {len(missing)} 个字符**字体里没有**（真机上会渲染成空白）：")
    for ch, locs in sorted(missing.items(), key=lambda kv: ord(kv[0])):
        print(f"    U+{ord(ch):04X}  {ch!r}   出现在 {', '.join(sorted(set(locs))[:5])}")
    sys.exit(1)
PY

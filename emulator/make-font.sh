#!/bin/bash
# 从文泉驿微米黑裁出中日文字体子集，输出到 fonts/。
#
# 为什么需要这个脚本：字体是 include_bytes! 编进二进制的，覆盖中日文是硬要求
# （见 fonts/README.md —— 用只有拉丁字母的字体会让界面看起来是坏的）。
# 这个脚本让「字体从哪来、包含什么」是可复现的，而不是一个来历不明的 blob。
#
# 用法：emulator/make-font.sh
#
# 前置：apt-get install -y fonts-wqy-microhei python3-fonttools

set -euo pipefail

SRC=/usr/share/fonts/truetype/wqy/wqy-microhei.ttc
FONTS_DIR=${FONTS_DIR:-./fonts}
DST=$FONTS_DIR/WenQuanYiMicroHei-Subset.ttf
TMP=/tmp/fontsub

if [ ! -f "$SRC" ]; then
    echo "找不到 $SRC"
    echo "先装：apt-get install -y fonts-wqy-microhei python3-fonttools"
    exit 1
fi

mkdir -p "$FONTS_DIR"
rm -rf "$TMP"; mkdir -p "$TMP"

echo "=== 1. 从 .ttc 里抽出第 0 面 ==="
python3 - "$SRC" "$TMP/base.ttf" <<'PY'
import sys
from fontTools.ttLib import TTCollection

src, out = sys.argv[1], sys.argv[2]
coll = TTCollection(src)
print(f"  集合里有 {len(coll.fonts)} 张面：")
for i, f in enumerate(coll.fonts):
    print(f"    [{i}] {f['name'].getDebugName(1) or '?'}  {len(f.getBestCmap())} 码位")
coll.fonts[0].save(out)
print(f"  已抽出第 0 面 -> {out}")
PY

echo
echo "=== 2. 裁子集 ==="
python3 - "$TMP/base.ttf" "$DST" <<'PY'
import sys
from fontTools import subset

src, dst = sys.argv[1], sys.argv[2]

# 覆盖中日文漫画标题 + 界面文案 + 数字标点所需的最小集
RANGES = [
    (0x0020, 0x007E),  # 基本拉丁
    (0x00A0, 0x00FF),  # 拉丁补充
    (0x2000, 0x206F),  # 常用标点（含 …、—）
    (0x3000, 0x303F),  # CJK 标点（、。「」《》）
    (0x3040, 0x309F),  # 平假名
    (0x30A0, 0x30FF),  # 片假名
    (0x3400, 0x4DBF),  # CJK 扩展 A
    (0x4E00, 0x9FFF),  # CJK 统一表意（主区）
    (0xFF00, 0xFFEF),  # 全角/半角形式
]
unicodes = []
for a, b in RANGES:
    unicodes += list(range(a, b + 1))

subset.main([
    src,
    "--unicodes=" + ",".join(f"U+{u:04X}" for u in unicodes),
    "--output-file=" + dst,
    "--drop-tables+=DSIG",
    "--no-hinting",         # 电子墨水屏上 hinting 意义不大，去掉省体积
    "--desubroutinize",
    "--layout-features=",   # 不做复杂排版，GSUB/GPOS 不需要
])
print("  完成")
PY

echo
echo "=== 3. 结果核对 ==="
ls -la "$DST" | awk '{printf "  文件 %s  %.1f MB\n", $9, $5/1048576}'
python3 - "$DST" <<'PY'
import sys
from fontTools.ttLib import TTFont

cmap = TTFont(sys.argv[1]).getBestCmap()
tests = {"中":0x4E2D, "漫":0x6F2B, "画":0x753B, "第":0x7B2C, "页":0x9875,
         "本":0x672C, "の":0x306E, "ア":0x30A2, "A":0x41, "1":0x31,
         "、":0x3001, "。":0x3002, "…":0x2026}
missing = [n for n, cp in tests.items() if cp not in cmap]
print(f"  码位数 {len(cmap)}")
if missing:
    print(f"  ⚠️ 缺字符：{','.join(missing)}")
    sys.exit(1)
print("  关键字符全有 ✓")
PY

echo
echo "别忘了重新构建：emulator/build-emulator.sh"

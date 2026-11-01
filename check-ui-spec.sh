#!/bin/bash
# 按设计规格自动验收 —— 把「能机器查的」都查一遍。
#
# 为什么需要它：这份规格里有好几条是**违反了也不报错、只是看着不对**的
# （符号字形掉成空白、边框细了被采样吃掉、候选栏高度变了导致整屏重排）。
# 靠人眼看截图容易漏，机器查一秒钟的事。
#
# 用法：bash check-ui-spec.sh        （改了 ui/*.slint 之后跑一下）
set -u
cd "$(dirname "$0")/ui" || exit 1
FAIL=0

echo "=== 1. 符号字形（✕ ▶ ⌫ ★ ← → 之类）—— 文泉驿里没有，会掉成空白 ==="
# 只查字符串字面量里的，注释里提到这些字符是在解释「为什么不用」
HITS=$(cat *.slint | sed 's|//.*||' | grep -nE '"[^"]*[✕▶◀⌫★☆←→↑↓✔✓✗※○●■□△▽〈〉]' || true)
if [ -n "$HITS" ]; then echo "$HITS"; FAIL=1; else echo "  （干净：界面文字里没有符号字形）"; fi

echo
echo "=== 2. 动画 / 过渡 / 定时器 —— 墨水屏上会留一串残影 ==="
HITS=$(cat *.slint | sed 's|//.*||' | grep -nE 'animate |Timer *\{|easing:' || true)
if [ -n "$HITS" ]; then echo "$HITS"; FAIL=1; else echo "  （干净：没有 animate / Timer / easing）"; fi

echo
echo "=== 3. 滚动 —— 规格要求列表一律分页 ==="
HITS=$(cat *.slint | sed 's|//.*||' | grep -nE 'ScrollView|Flickable|Scrollable|TouchArea.*moved' || true)
if [ -n "$HITS" ]; then echo "$HITS"; FAIL=1; else echo "  （干净：没有滚动容器）"; fi

echo
echo "=== 4. 底栏五项顺序（历史/搜索/主页/收藏/设置）==="
grep -n 'Ico\(History\|Search\|Home\|Star\|Settings\)' chrome.slint | head -5

echo
echo "=== 5. 阅读器有没有残留的常驻按钮/状态条 ==="
awk '/screen == 8 : Rectangle/,/screen == 9 : Rectangle/' app.slint \
  | sed 's|//.*||' | grep -nE 'NavBar|StatusBar|AppBar|PagerBar' || echo "  （干净：阅读器里没有常驻栏）"

echo
echo "=== 6. 候选词栏：高度恒定 + v6 横排一行 ==="
# v6 尺寸图：142px 高，108 + 204×5 + 108 = 1236
grep -n 'height: 142px' app.slint | head -3 || true
if grep -q 'for c\[i\] in root.cand-row-a' app.slint; then
  echo "  OK   候选是一行渲染（没有 row-b）"
else
  echo "  *** 找不到一行候选的渲染"; FAIL=1
fi
# 每格 204px（5 格 = 1020），加两端 108×2 = 1236
if grep -q 'width: 204px' app.slint && grep -q 'width: 108px' app.slint; then
  echo "  OK   候选格 204px / 尖角键 108px（108 + 204×5 + 108 = 1236）"
else
  echo "  *** 候选格或尖角键的宽度不对"; FAIL=1
fi
# 尖角必须是矢量画的，不是「<」「>」字符
if grep -qE 'text: *"[<>]"' app.slint; then
  echo "  *** 翻页还在用「<」「>」字符 —— 文泉驿里画不出来，要改成 IcoChev*"; FAIL=1
else
  echo "  OK   翻页用的是矢量尖角（IcoChevLeft / IcoChevRight）"
fi

echo
echo "=== 7. 键盘高度（v6 去掉上边框）==="
grep -n 'height: 463px' app.slint || { echo "  *** 键盘该是 463px（466 − 3px 上边框）"; FAIL=1; }

echo
echo "=== 8. 骨架高度（规格第 8 节）==="
grep -nE 'nav-h: 108px|pager-h: 140px|appbar-h: 128px|status-h: 88px' tokens.slint

echo
echo "=== 9. 禁用态 #999 有没有用上 ==="
grep -c 'Tok.g9' chrome.slint app.slint | sed 's/^/  /'

echo
echo "=== 11. 列表行的触摸区：行和星标必须**拼起来、不重叠** ==="
#
# 用户报过「目录旁边的收藏点不动」。根因是行级 TouchArea 铺满整行，
# 把星标那一下吞了。**靠声明顺序（z 序）压住它是不行的** —— 试过，没用，
# 重叠的两个 TouchArea 谁拿到事件不可靠。
# 现在改成两块拼起来：行触摸区到 star-hit-x 为止，星标热区接着铺到行尾。
LL=$(awk '/export component ListLine/,/^}/' chrome.slint)
if echo "$LL" | grep -q 'root.star-hit-x : root.width'; then
  echo "  OK   行触摸区让开了星标那一条（到 star-hit-x 为止）"
else
  echo "  *** 行触摸区没有让开星标 —— 星标会被吞掉点击"; FAIL=1
fi
if echo "$LL" | grep -q 'width: root.width - root.star-hit-x'; then
  echo "  OK   星标热区接着铺到行尾（两块正好拼满，不重叠）"
else
  echo "  *** 星标热区宽度不对，可能和行触摸区重叠"; FAIL=1
fi

echo
echo "=== 12. 列表行标题别画到星标底下（会字叠字）==="
# 标题宽度必须跟着星标/元信息的**实际位置**走。
# 之前写死「有元信息留 300、没有留 110」，而星标在 width−306，
# 章节行（无元信息、有星标）的标题就一路盖到星上。
if awk '/export component ListLine/,/^}/' chrome.slint | grep -q 'right-reserve'; then
  echo "  OK   标题宽度由 right-reserve 推导（和星标位置同源）"
else
  echo "  *** 标题宽度没走 right-reserve —— 容易和星标位置对不上"; FAIL=1
fi

echo
echo "=== 13. 必须有「退出程序」入口（漏了会把用户卡在应用里）==="
#
# 踩过一次：回调 `quit-requested` 声明了、Rust 也接好了，但**界面上没人调它** ——
# 用户进了应用就出不去，只能长按电源键强制重启。
# 编译通过、跑得好好的，只有真机上想退出时才发现。
if grep -q 'root.quit-requested()' app.slint; then
  N=$(grep -c 'root.quit-requested()' app.slint)
  echo "  OK   有 $N 处调用退出程序的入口"
else
  echo "  *** quit-requested 声明了但没人调 —— 用户出不去"; FAIL=1
fi

echo
echo "=== 14. 输入框光标宽度必须是偶数（奇数会随位置时粗时细）==="
#
# 光标是紧跟文字的竖条。渲染器对矩形边做 `round()` 取整：
#     物理宽 = round(x + W) − round(x)
# W 是整数时这个差**恒等于 W**；W 是小数时会随位置摇摆。
#
# 桌面预览缩放 0.5，逻辑宽 3px → 物理 1.5px → **在 1 和 2 之间交替**，
# 表现是「打几个字光标变细，再打几个字又变粗」。
# 实测过：3px 时逐字符量出来是 1 1 2 2 2 2 1 2 1 1 2 2 2 1 1。
#
# 取偶数则 0.5 和 1.0 两个缩放下的物理宽都是整数，怎么取整都一样。
# （和 README 第 7 条「边框不能是 1px」是同一类坑 —— 违反了不报错。）
CARET_W=$(awk '/width: *[0-9]+px/{w=$0} /height: *54px/{print w; exit}' app.slint)
CARET_N=$(echo "$CARET_W" | grep -oE '[0-9]+' | head -1)
if [ -z "$CARET_N" ]; then
  echo "  *** 找不到光标宽度（app.slint 里 height: 54px 那个 Rectangle）"; FAIL=1
elif [ $((CARET_N % 2)) -ne 0 ]; then
  echo "  *** 光标宽 ${CARET_N}px 是奇数 —— 桌面预览下会时粗时细，改成偶数"; FAIL=1
else
  echo "  OK   光标宽 ${CARET_N}px（偶数，0.5 和 1.0 两个缩放下都不抖）"
fi

echo
echo "=== 15. 底栏顶部的分界线必须露得出来（被格子盖住就白画了）==="
#
# 踩过一次：分界线声明在 5 个格子的**前面**，而格子写的是
# `y: 0; height: 整条高度`（整格铺满）—— 等于用白底把那条线整个刷掉。
#
# 现象是「底栏那 5 个板块上面没有那一横，跟上面的内容没有分界线」，
# 而代码里那条线明明写着，编译也不报错。实测：线该在物理 y 770/771
# 横贯 618px，实际那两行只有 122 个暗像素（全是图标）。
#
# 所以：格子必须让开顶上那 3px（`y: Tok.border`）。
#
# ⚠️ `sed 's|//.*||'` 剥注释**不能省**：上面这段说明里就写着 `y: Tok.border`，
#    不剥的话注释自己会把检查喂饱 —— 代码改回 `y: 0` 也照样「通过」（假绿灯）。
#    这条是做完负向验证才发现的：第一版加了注释就抓不到错误了。
NAV=$(awk '/export component NavBar/,/^}/' chrome.slint | sed 's|//.*||')
if echo "$NAV" | grep -q 'y: Tok.border'; then
  echo "  OK   底栏格子让开了顶上 3px，分界线露得出来"
else
  echo "  *** 底栏格子从 y:0 铺满整格 —— 会把顶上的分界线盖掉"; FAIL=1
fi

echo
echo "=== 16. 每一屏都要有底栏；出错页还要有「退出程序」 ==="
#
# 踩过两次同一类坑，都是「声明了但界面上够不着」：
#   ① HANDOVER 1.7：`quit-requested` 回调接好了、Rust 也接好了，
#      但**界面上没有任何地方调它** —— 用户进了应用就出不去。
#   ② 出错页既没有底栏、也没有退出入口，而全站的退出只在**主页右上角**。
#      断网时用户到不了主页 → 连着重试都失败之后只能长按电源键强制重启。
#
# 底栏是常驻导航（阅读器除外，它是真·全屏），少一屏就意味着
# 「导航栏突然不见了」——用户会以为程序坏了。
#
# ⚠️ `sed 's|//.*||'` 剥注释不能省：上面这些说明里就写着 "NavBar" 和
#    "quit-requested"，不剥的话注释自己会把检查喂饱（假绿灯，踩过）。
# 0..7 是普通屏，9 是出错页；8 是阅读器，**刻意没有底栏**。
for n in 0 1 2 3 4 5 6 7 9; do
  BLK=$(awk -v n="$n" '$0 ~ ("if root.screen == " n " : Rectangle"), /^    \}$/' app.slint \
        | sed 's|//.*||')
  if [ -z "$BLK" ]; then
    echo "  *** 找不到第 $n 屏（screen == $n 的块）—— 检查脚本本身要更新了"; FAIL=1
  elif echo "$BLK" | grep -q 'NavBar *{'; then
    echo "  OK   第 $n 屏有底栏"
  else
    echo "  *** 第 $n 屏没有底栏 —— 用户会以为导航栏不见了"; FAIL=1
  fi
done

# 出错页额外要求：必须自己能退出。别的屏够不着主页时，就只剩这一条路。
ERRBLK=$(awk '$0 ~ "if root.screen == 9 : Rectangle", /^    \}$/' app.slint | sed 's|//.*||')
if echo "$ERRBLK" | grep -q 'root.quit-requested()'; then
  echo "  OK   出错页有「退出程序」入口"
else
  echo "  *** 出错页没有退出入口 —— 断网时用户出不去，只能长按电源键重启"; FAIL=1
fi

echo
echo "=== 17. 搜索结果必须是三列封面网格（图片为主）==="
# 用户明确要求「搜索列表改成图片为主的」。改回 ListLine 列表也算错。
# 三列靠 Math.mod(i, 3) 排（Slint 没有 % 运算符）。
SEARCHBLK=$(awk '$0 ~ "if root.screen == 1 : Rectangle", /^    \}$/' app.slint | sed 's|//.*||')
if echo "$SEARCHBLK" | grep -q 'CoverCell'; then
  echo "  OK   搜索页用 CoverCell 渲染"
else
  echo "  *** 搜索页没有 CoverCell —— 又退回文字列表了"; FAIL=1
fi
if echo "$SEARCHBLK" | grep -q 'Math.mod(i, 3)'; then
  echo "  OK   三列排版（Math.mod(i, 3)）"
else
  echo "  *** 找不到三列排版 —— 检查 Math.mod(i, 3)"; FAIL=1
fi

echo
echo "=== 18. 必须有全局加载圈（翻页/搜索要几秒，不能毫无反馈）==="
#
# 原来查的是「阅读器那块里有没有 root.loading」。后来加载反馈统一成了
# **全站一个右下角的转圈**（SpinRing），阅读器那块居中面板删掉了 ——
# 所以这条改成查「转圈覆盖层还在不在」，而不是查某一屏里的局部标记。
#
# ⚠️ 仍然要剥注释：上面这几行说明里就写着 `root.loading` 和 `SpinRing`。
ALL=$(cat *.slint | sed 's|//.*||')
if echo "$ALL" | grep -q 'if root.spin-shown : SpinRing'; then
  echo "  OK   有全局加载圈覆盖层（if root.spin-shown : SpinRing）"
else
  echo "  *** 找不到加载圈覆盖层 —— 加载期间屏幕上什么都不会变"; FAIL=1
fi

# 转圈必须真的会「变」：Rust 侧要有推进相位的定时消息，否则就是一个静止的圈。
if grep -q 'SPIN_INTERVAL_MS' ../src/main.rs && grep -q 'Cmd::SpinTick' ../src/main.rs; then
  echo "  OK   Rust 侧有相位推进（SpinTick）"
else
  echo "  *** 转圈不会动 —— 找不到 SpinTick 推进逻辑"; FAIL=1
fi

# 软件渲染器不支持 rotation-angle，转圈只能靠预生成的多段路径。
if grep -q 'ArcPaths' chrome.slint && grep -q 'commands: ArcPaths.all' chrome.slint; then
  echo "  OK   转圈用预生成的圆弧路径（软件渲染器不支持 rotation-angle）"
else
  echo "  *** 找不到 ArcPaths —— 转圈可能用上了 rotation-angle，真机上不会转"; FAIL=1
fi

echo
echo "=== 19. 详情页标签区不能压到目录 ==="
#
# 标签区（角色 + 标签）最多 3 行 = 226px，从 y=496 排到 722；
# 目录标题在 744。这个间距是算出来的，改任何一边都要重算 ——
# 历史上就因为「标签高度 52 / 界面画 66」两边不一致，两行标签多算
# 28px，正好压到目录第一行上。
# ⚠️ 剥注释不能省：上面这段说明里就写着 y=496 / 744。
DETAIL=$(awk '$0 ~ "if root.screen == 7 : Rectangle", /^    \}$/' app.slint | sed 's|//.*||')
TAG_Y=$(echo "$DETAIL" | grep -oE '^\s+y: 496px' | head -1)
DIR_Y=$(echo "$DETAIL" | grep -oE '^\s+y: 744px' | head -1)
if [ -n "$TAG_Y" ] && [ -n "$DIR_Y" ]; then
  echo "  OK   标签区 y=496、目录标题 y=744（中间留 22px 余量）"
else
  echo "  *** 详情页的标签区/目录标题坐标变了 —— 重新算一遍间距"; FAIL=1
fi

# 章节区最后一行不能越过翻页条（1400）
if echo "$DETAIL" | grep -qE 'y: 800px \+ i \* 100px'; then
  echo "  OK   章节 6 行 × 100px 从 800 起 → 1400，正好停在翻页条上"
else
  echo "  *** 章节行的起点/行高变了 —— 确认最后一行没被翻页条盖住"; FAIL=1
fi

# 分组标题和标签必须是两个模型（混在一起会让点击索引错位）
if grep -q 'tag-headers' chrome.slint 2>/dev/null || grep -q 'in property <\[TagHeader\]>' app.slint; then
  echo "  OK   分组标题是独立模型（tag-headers），不和 TagChip 混用"
else
  echo "  *** 找不到 tag-headers —— 标题可能混进 TagChip，点击会搜错词"; FAIL=1
fi

echo
echo "=== 20. 「全部标签」弹窗（详情页末行的「详情」打开）==="
#
# 详情页只有三行标签，超出的靠这个弹窗看全。三件事都要在：
#   1. 详情页有「详情」格子（tag-more）
#   2. 有弹窗本体（detail-shown 覆盖层，圆角）
#   3. 弹窗里的标签点了也走 tag-activated（不是死链）
# ⚠️ 剥注释不能省：下面这段说明里就写着 tag-more / detail-shown。
ALL2=$(cat *.slint | sed 's|//.*||')
if echo "$ALL2" | grep -q 'root.tag-more'; then
  echo "  OK   详情页有「详情」格子"
else
  echo "  *** 找不到 tag-more —— 弹窗没有入口"; FAIL=1
fi
if echo "$ALL2" | grep -q 'if root.detail-shown : Rectangle'; then
  echo "  OK   有「全部标签」弹窗覆盖层"
else
  echo "  *** 找不到 detail-shown 覆盖层"; FAIL=1
fi
if echo "$ALL2" | grep -q 'border-radius: 24px'; then
  echo "  OK   弹窗是圆角窗口（border-radius: 24px）"
else
  echo "  *** 弹窗没有圆角"; FAIL=1
fi
DETAILCALL=$(echo "$ALL2" | awk '/if root.detail-shown : Rectangle/,/^    \}$/')
if echo "$DETAILCALL" | grep -q 'root.tag-activated'; then
  echo "  OK   弹窗里的标签可点（走 tag-activated）"
else
  echo "  *** 弹窗里的标签点不了"; FAIL=1
fi

echo
echo "=== 10. 六档灰之外的颜色（规格 2.1）==="
for f in tokens.slint icons.slint chrome.slint app.slint; do
  H=$(sed 's|//.*||' "$f" | grep -oE '#[0-9a-fA-F]{3,8}' | sort -u \
      | grep -viE '^#(000000|555555|888888|999999|cccccc|ffffff)$' || true)
  if [ -n "$H" ]; then echo "  *** $f 用了六档灰之外的颜色：$H"; FAIL=1; fi
done
[ "$FAIL" = "0" ] && echo "  （干净：只用 tokens.slint 那六档）"

echo
if [ "$FAIL" = "0" ]; then
  echo "✅ 规格检查全部通过"
else
  echo "❌ 有不符合规格的地方（见上面 ***）"
fi
exit $FAIL

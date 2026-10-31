# 字体

字体是被 `include_bytes!` **编进二进制**的，不是运行时加载的文件。

## 为什么必须打包字体

Kindle 系统里既没有 fontconfig，也没有默认字体路径。Slint 的软件渲染器在第一次
做字体回退查询时会直接 panic。所以后端 `slint_backend_kindle::install(FONT)`
强制要求传一份字体，没有就用不起来。

## ⚠️ 字体必须覆盖中日文

这不是「好看不好看」的问题，是**能不能显示**的问题。

最初用的是 `LiberationSans-Regular.ttf`，它一共只有 **2327 个码位**，全是
拉丁/希腊/西里尔字母 —— **一个汉字、一个假名都没有**。而漫画的本子标题、
章节名几乎全是中日文，于是整个界面看起来像坏了：标题位置一片空白，
但日志里标题明明是对的。

实测对比（同一套流程，只换字体）：

| 截图 | LiberationSans | 现在这个字体 |
|---|---|---|
| 首页 | 15 KB | 31 KB |
| 最近观看 | 25 KB | 88 KB |
| 章节目录 | 7 KB | 36 KB |

（PNG 体积和「墨迹像素」数都说明同一件事：文字从没画出来变成了画出来。）

**别再换回只有拉丁字母的字体。** 如果要换，先确认它覆盖 U+4E00..U+9FFF。

## 当前字体：WenQuanYiMicroHei-Subset.ttf

- **来源**：Ubuntu 的 `fonts-wqy-microhei` 包里的 `wqy-microhei.ttc` 第 0 面
  （文泉驿微米黑，Apache 2.0 许可）
- **大小**：3.0 MB，21553 个码位
- **为什么不用 Noto Sans CJK**：完整版 `NotoSansCJK-Regular.ttc` 是 **19 MB**，
  编进二进制太大。文泉驿微米黑只有 4.9 MB，裁完 3 MB，中日文覆盖够用。
- **为什么裁子集**：
  1. 原文件是 `.ttc` 字体集合（两张面：Micro Hei 和 Micro Hei Mono），
     从路径加载时取哪一面不确定；裁成单面 `.ttf` 行为确定
  2. 原始文件里有一堆用不到的东西（泰文、越南文、大量符号），
     这个字体是编进二进制的，每 1 MB 都直接加到二进制体积上

保留的区段（覆盖中日文漫画标题 + 界面文案 + 数字标点）：

```
U+0020-U+007E   基本拉丁
U+00A0-U+00FF   拉丁补充
U+2000-U+206F   常用标点（含 …、—）
U+3000-U+303F   CJK 标点（、。「」《》）
U+3040-U+309F   平假名
U+30A0-U+30FF   片假名
U+3400-U+4DBF   CJK 扩展 A
U+4E00-U+9FFF   CJK 统一表意（主区）
U+FF00-U+FFEF   全角/半角形式
```

## 怎么重新生成

```bash
apt-get install -y fonts-wqy-microhei python3-fonttools
bash emulator/make-font.sh
```

## LiberationSans-Regular.ttf

这个文件现在**没有被用到**（`src/main.rs` 里不再引用）。留着只是备查，
**不要拿它当默认字体** —— 它就是上面那个「中文全空白」问题的成因。

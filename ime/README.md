# 中文拼音输入法数据

这个目录是输入法的**数据来源和转换脚本**。运行时的查表逻辑在 `src/ime.rs`。

## 文件

| 文件 | 大小 | 说明 |
|---|---|---|
| `zh_pinyin_data.lua` | 890 KB | **原始数据**，从 Kindle 上 KOReader 拷来的 |
| `gen_table.py` | 9.5 KB | 转换脚本，Lua → 紧凑二进制 |
| `pinyin.bin` | **1.19 MB** | 生成物，`include_bytes!` 编进二进制 |

## 数据出处与授权

`zh_pinyin_data.lua` 来自 Kindle 上已装的 KOReader：

```
/mnt/us/koreader/frontend/ui/data/keyboardlayouts/zh_pinyin_data.lua
```

文件头一行写明出处是 [rime-pinyin-simp](https://github.com/rime/rime-pinyin-simp)，
**Apache License 2.0**，可自由使用。

**它是从那台 Kindle 上现成拷下来的，不依赖网络。** 想更新就重新 `cp` 一次，
再跑 `gen_table.py`。

## 这份表里到底有什么（和旧文档说的不一样）

交接文档里写的是「单字级，没有词组」—— **这是错的**，实测：

| | 数量 |
|---|---|
| 条目（拼音串 → 候选列表） | **38,998** |
| 单字候选 | 9,534 |
| **词组候选** | **48,056** |
| 其中「音节」条目 | 415 |

只有 415 个条目是纯音节（`yuan`、`shen` 这种），**其余三万八千多条都是词组**：

```
chengjiu = 成就
eryanghualiu = 二氧化硫
zhongwen = 中文
```

所以「原神要按两次」这个判断也不成立 —— 表里有 `yuanshen`（元神／原审）。
只不过它给的候选未必是用户想要的那个，所以输入法还有一层**分词回退**
（见 `src/ime.rs`）：把 `yuanshen` 切成 `yuan|shen` 再组合，就能拼出「原神」。

## 二进制格式

全部小端。表头 32 字节，然后索引区、key 区、value 区依次排列。

```
偏移  类型  含义
 0    u8[4] magic "JMPY"
 4    u8    version = 1
 5    u8    flags = 0
 6    u16   pad
 8    u32   count        条目数
12    u32   index_off    索引区起始（= 32）
16    u32   key_off      key 区起始
20    u32   key_len      key 区字节数
24    u32   val_off      value 区起始
28    u32   val_len      value 区字节数
```

**索引项 12 字节 × count**（key 已按字节序升序，客户端直接二分）：

```
+0  u32  key_off   该 key 在文件里的绝对偏移（NUL 结尾的 ASCII）
+4  u32  val_off   第一个候选的绝对偏移（NUL 结尾的 UTF-8）
+8  u16  val_n     候选个数（候选在 value 区里连续排列）
+10 u8   flags     bit0 = 这是个「音节」key
+11 u8   pad
```

### 为什么要「音节」标志位

分词回退要把 `yuanshen` 切成 `yuan|shen`，就得知道哪些 key 是合法音节。

判别规则是**长度 ≤ 6 且候选里至少有一个单字**，全表命中 415 个
（汉语拼音音节总数约 410，多出来的是 `xian` / `yuan` / `jian` 这种既收单字
又收词组的）。注意不能要求「候选全是单字」—— 那样这 17 个会被误判成非音节，
`xianzai` 就切不开了。

`gen_table.py` 里对音节数量有断言（350~450），数据换了、规则失效会立刻报错。

## 为什么在构建期转换，而不是运行时解析 Lua

1. 890 KB 的文本每次启动解析一遍，在 Kindle 上要好几秒 —— 而且正卡在启动路径上
2. Lua 解析器本身要进二进制，比这张表还难维护
3. 转出来的表是 `include_bytes!` 的 `.rodata`，**内存映射、不占堆**；
   gzip 反而要先解到堆上，还得保证解压缓冲放得下

## 字体覆盖

候选栏里的生僻字必须字体画得出来，否则是一排豆腐块。
`gen_table.py` 会读 `fonts/WenQuanYiMicroHei-Subset.ttf` 的 cmap，
**自动丢掉画不出来的候选**。

实测 9,248 个候选字符里只有 **1 个**画不出来（`㳇` U+3CC7，CJK 扩展 A 的边缘，
在 `hufu` 条目的「湖㳇」里），已自动过滤。

## 怎么重新生成

```bash
# 从 Kindle 重新拷原始数据（可选，只有想更新词库时才需要）
cp /mnt/k/koreader/frontend/ui/data/keyboardlayouts/zh_pinyin_data.lua \
   ime/zh_pinyin_data.lua

# 转换
python3 ime/gen_table.py
```

需要 `fonts/WenQuanYiMicroHei-Subset.ttf` 存在（读 cmap 用），构建前没生成过字体
就先跑 `emulator/make-font.sh`。

改完要重新构建二进制才生效 —— 表是编译期 `include_bytes!` 进去的。

## 验证

```bash
# 单元测试：表结构、二分查找、分词回退、翻页边界
cargo test --target x86_64-unknown-linux-gnu

# 模拟器端到端：打拼音 → 出候选 → 点字上屏，并逐格核对字形不是豆腐块
emulator/ime-test.sh
```

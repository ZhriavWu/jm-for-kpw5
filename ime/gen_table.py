#!/usr/bin/env python3
"""把 KOReader 的 zh_pinyin_data.lua 转成客户端能直接 include_bytes! 的紧凑表。

为什么要在构建期转换，而不是运行时解析 Lua 文本：
  1. 910 KB 的文本每次启动解析一遍，在 Kindle 上要好几秒 —— 而且这段
     时间正好卡在启动路径上
  2. 解析器本身要进二进制，比表还难维护
  3. 转出来的表是**内存映射**的 .rodata，不占堆；gzip 反而要解到堆上

数据来源：
  KOReader  frontend/ui/data/keyboardlayouts/zh_pinyin_data.lua
  原始出处 rime-pinyin-simp，Apache License 2.0

产出：ime/pinyin.bin（约 1.1 MB）

二进制格式（全部小端）：
    offset 0   magic "JMPY"
    offset 4   u8  version = 1
    offset 5   u8  flags   = 0
    offset 6   u16 pad
    offset 8   u32 count          条目数（key 已按字节序升序）
    offset 12  u32 index_off      索引区起始
    offset 16  u32 key_off        key 区起始
    offset 20  u32 key_len        key 区字节数
    offset 24  u32 val_off        value 区起始
    offset 28  u32 val_len        value 区字节数

    索引项 12 字节 × count：
        u32 key_off   该 key 在文件里的绝对偏移（NUL 结尾的 ASCII）
        u32 val_off   第一个候选在文件里的绝对偏移（NUL 结尾的 UTF-8）
        u16 val_n     候选个数（候选在 value 区里连续排列）
        u8  flags     bit0 = 这是「音节」key
        u8  pad

「音节」标志位是给客户端的分词回退用的：
    `yuanshen` 在表里是个整词（元神/原审），但用户想要的常常是
    「原」+「神」。客户端把缓冲切成若干合法音节再拼组合，就能覆盖这种情况。
切分需要知道哪些 key 是音节 —— 规则是「长度 ≤ 6 且候选里至少有一个单字」，
全表命中 415 个（汉语拼音音节总数约 410，多出来的是 xian/yuan/jian 这种
既收单字又收词组的）。这条规则在下面有断言兜底，数据换了会立刻报错。

key 区是 ASCII（纯小写字母），按字节序升序 —— 客户端直接二分。
value 区里候选按词频排序，保留原始顺序。

用法：python3 ime/gen_table.py
"""

import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
LUA = os.path.join(HERE, "zh_pinyin_data.lua")
FONT = os.path.join(ROOT, "fonts", "WenQuanYiMicroHei-Subset.ttf")
OUT = os.path.join(HERE, "pinyin.bin")

MAGIC = b"JMPY"
VERSION = 1
HEADER_LEN = 32
IDX_ENTRY = 12
FLAG_SYLLABLE = 1

# 音节 key 的长度上限。最长的拼音音节是 6 个字母（zhuang / chuang / shuang）
SYLLABLE_MAX_LEN = 6
# 音节数量的合理区间，用来兜住「判别规则失效」这种情况
SYLLABLE_MIN, SYLLABLE_MAX = 350, 450

ENTRY_RE = re.compile(r'([a-z]+)\s*=\s*(?:\{([^}]*)\}|"([^"]*)")')
VALUE_RE = re.compile(r'"([^"]*)"')


# ------------------------------------------------------------------ 字体

def font_codepoints(path):
    """读 TTF 的 cmap，返回它能画出来的所有码位。

    候选栏要显示的生僻字（「灞」「龅」这种）必须在这个集合里，
    否则界面上是一排豆腐块。字体子集是按码位区间裁的，实际只差一个字符，
    但让这一步自动过滤比手工记着更可靠。
    """
    data = open(path, "rb").read()
    n = struct.unpack(">H", data[4:6])[0]
    tables = {}
    for i in range(n):
        off = 12 + i * 16
        tag = data[off:off + 4].decode("latin1")
        toff, tlen = struct.unpack(">II", data[off + 8:off + 16])
        tables[tag] = toff

    cmap = tables["cmap"]
    n_sub = struct.unpack(">H", data[cmap + 2:cmap + 4])[0]
    best = None
    for i in range(n_sub):
        rec = cmap + 4 + i * 8
        pid, eid, sub = struct.unpack(">HHI", data[rec:rec + 8])
        if (pid, eid) in ((3, 1), (3, 10), (0, 3), (0, 4)):
            best = cmap + sub
    if best is None:
        raise SystemExit("字体里找不到可用的 cmap 子表")

    fmt = struct.unpack(">H", data[best:best + 2])[0]
    covered = set()
    if fmt == 4:
        seg_x2 = struct.unpack(">H", data[best + 6:best + 8])[0]
        seg = seg_x2 // 2
        ends = struct.unpack(f">{seg}H", data[best + 14:best + 14 + seg_x2])
        starts = struct.unpack(f">{seg}H", data[best + 16 + seg_x2:best + 16 + seg_x2 * 2])
        for s, e in zip(starts, ends):
            if s != 0xFFFF:
                covered.update(range(s, e + 1))
    elif fmt == 12:
        ngroups = struct.unpack(">I", data[best + 12:best + 16])[0]
        for i in range(ngroups):
            p = best + 16 + i * 12
            s, e, _ = struct.unpack(">III", data[p:p + 12])
            covered.update(range(s, e + 1))
    else:
        raise SystemExit(f"不支持的 cmap 格式 {fmt}")
    return covered


# ------------------------------------------------------------------ 解析

def parse_lua(path):
    """解析 Lua 表。两种条目形式：key={...} 和 key="单个词"。"""
    text = open(path, "rb").read().decode("utf-8")
    if "return {" not in text:
        raise SystemExit(f"{path} 里找不到 return {{")
    if "\\" in text:
        # 目前的数据没有任何转义；真有的话得先处理再解析，不能默默放过
        raise SystemExit(f"{path} 里出现了反斜杠转义，解析规则需要更新")

    _, _, body = text.partition("return {")
    entries = []
    seen = set()
    for m in ENTRY_RE.finditer(body):
        key, lst, single = m.group(1), m.group(2), m.group(3)
        values = VALUE_RE.findall(lst) if lst is not None else [single]
        if not values:
            continue
        if key in seen:
            raise SystemExit(f"key 重复：{key}")
        seen.add(key)
        entries.append((key, values))
    return entries


def is_syllable(key, values):
    """这个 key 能不能当「音节」用来切分。

    判别规则：长度 ≤ 6 且候选里至少有一个单字。不能要求「全是单字」——
    xian / yuan / jian 这些音节同时也收了词组（西安、预案、吉安），
    那样会被误判成非音节，`xianzai` 就切不开了。
    """
    return len(key) <= SYLLABLE_MAX_LEN and any(len(v) == 1 for v in values)


def clean(entries, covered):
    """丢掉字体画不出来的候选，去掉同一 key 里的重复候选。"""
    dropped = []
    out = []
    for key, values in entries:
        kept = []
        for v in values:
            if any(ord(c) not in covered for c in v):
                dropped.append((key, v))
                continue
            if v not in kept:
                kept.append(v)
        if kept:
            out.append((key, kept))
    return out, dropped


# ------------------------------------------------------------------ 打包

def build(entries):
    entries.sort(key=lambda kv: kv[0].encode())

    count = len(entries)
    index_off = HEADER_LEN
    key_off = index_off + count * IDX_ENTRY

    key_blob = bytearray()
    key_pos = {}
    for key, _ in entries:
        key_pos[key] = key_off + len(key_blob)
        key_blob += key.encode("ascii") + b"\0"

    val_off = key_off + len(key_blob)
    val_blob = bytearray()
    idx = bytearray()
    for key, values in entries:
        vstart = val_off + len(val_blob)
        for v in values:
            val_blob += v.encode("utf-8") + b"\0"
        flags = FLAG_SYLLABLE if is_syllable(key, values) else 0
        idx += struct.pack("<IIHBB", key_pos[key], vstart, len(values), flags, 0)

    header = MAGIC + struct.pack(
        "<BBHIIIIII",
        VERSION, 0, 0,
        count, index_off,
        key_off, len(key_blob),
        val_off, len(val_blob),
    )
    assert len(header) == HEADER_LEN, len(header)
    return header + bytes(idx) + bytes(key_blob) + bytes(val_blob)


def main():
    for p in (LUA, FONT):
        if not os.path.exists(p):
            raise SystemExit(f"找不到 {p}")

    print(f"来源 {LUA}")
    entries = parse_lua(LUA)
    print(f"  解析出 {len(entries):,} 条")
    n_vals = sum(len(v) for _, v in entries)
    n_chars = sum(1 for _, vs in entries for v in vs if len(v) == 1)
    print(f"  候选 {n_vals:,} 个（单字 {n_chars:,} / 词组 {n_vals - n_chars:,}）")

    covered = font_codepoints(FONT)
    print(f"字体 {os.path.basename(FONT)} 覆盖 {len(covered):,} 码位")
    entries, dropped = clean(entries, covered)
    if dropped:
        print(f"  过滤掉字体画不出的候选 {len(dropped)} 个：", dropped[:10])
    n_vals = sum(len(v) for _, v in entries)
    print(f"  清理后 {len(entries):,} 条 / {n_vals:,} 个候选")

    n_syl = sum(1 for k, vs in entries if is_syllable(k, vs))
    print(f"  其中音节 key {n_syl:,} 个（分词回退用）")
    if not SYLLABLE_MIN <= n_syl <= SYLLABLE_MAX:
        raise SystemExit(
            f"音节判别规则可能失效了：得到 {n_syl} 个，"
            f"预期在 {SYLLABLE_MIN}~{SYLLABLE_MAX} 之间"
        )

    blob = build(entries)
    with open(OUT, "wb") as f:
        f.write(blob)
    print(f"\n写出 {OUT}")
    print(f"  {len(blob):,} 字节（{len(blob)/1024/1024:.2f} MB）")
    print(f"  其中 key 区 {sum(len(k)+1 for k,_ in entries):,} B、"
          f"value 区 {sum(len(v.encode())+1 for _,vs in entries for v in vs):,} B、"
          f"索引 {len(entries)*IDX_ENTRY:,} B")


if __name__ == "__main__":
    sys.exit(main())

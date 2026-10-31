//! 中文拼音输入法：查表 + 候选组装。
//!
//! # 数据从哪来
//!
//! `ime/pinyin.bin` 是构建期由 `ime/gen_table.py` 从 KOReader 的
//! `zh_pinyin_data.lua`（原始出处 rime-pinyin-simp，Apache-2.0）转出来的紧凑表，
//! 用 `include_bytes!` 编进二进制。**不要运行时解析 Lua** —— 910 KB 的文本
//! 每次启动解一遍在 Kindle 上要好几秒，而且解析器本身比表还难维护。
//!
//! 表的二进制格式见 `ime/gen_table.py` 顶部注释。
//!
//! # 查表规则
//!
//! 输入一串拼音字母（`buf`），按优先级：
//!
//!   1. **整串精确命中** —— `yuan` → 元远原源员园…
//!   2. **整串前缀命中** —— `eryang` 不是词，但 `eryanghualiu`（二氧化硫）是，
//!      于是拿它的候选。这是「打一半就出结果」
//!   3. **分词回退** —— 把缓冲切成若干个合法音节再拼组合。
//!      这一步是给「整串命中但候选不对」准备的：`yuanshen` 确实是个词
//!      （元神／原审），但用户打的往往是「原」+「神」。
//!
//! 第 3 步对纯音节输入（`yuan` 只有一个音节）不产生任何东西，所以不会干扰第 1 步。
//!
//! # 为什么不直接抄 KOReader 的 generic_ime.lua
//!
//! 那套是给「屏幕键盘 + ← → 翻候选」设计的：状态机是栈式的，还带通配符、
//! 大小写跟随、自动分词回调。我们这边是触摸屏，点候选直接上屏，用不着栈 ——
//! 一个缓冲串加一个候选列表就够了。少一层状态机，出错的地方也少一处。

/// 词典表。由 ime/gen_table.py 生成，格式见那个脚本的顶部注释。
static TABLE: &[u8] = include_bytes!("../ime/pinyin.bin");

const MAGIC: [u8; 4] = *b"JMPY";
const HEADER_LEN: usize = 32;
const IDX_ENTRY: usize = 12;
/// 索引项 flags 的 bit0：这个 key 能当音节用（分词回退的切分依据）
const FLAG_SYLLABLE: u8 = 1;
/// 最长音节 `zhuang` 的字母数
const SYLLABLE_MAX_LEN: usize = 6;

/// 候选词栏一页放几个。
///
/// **v6：横排一行 5 个**（照 Kindle 系统输入法），
/// 界面那边是 `108 + 204 × 5 + 108 = 1236`（见 `ui/app.slint` 的候选词栏）。
/// **改这里必须同步改那边** —— 两处不一致的表现是最后一格点不到、
/// 或者栏里留一个空格子。
pub const PAGE: usize = 5;
/// 候选总数上限。同音字最多的是 `yi`，106 个；再加分词组合，160 够用。
const MAX_CANDS: usize = 160;
/// 分词回退：每段取词频最高的前几个单字参与组合
const COMBO_PER_PART: usize = 3;
/// 分词回退：最多切成几段
const MAX_PARTS: usize = 3;
/// 分词回退：最多考虑几组切分
const MAX_SEGS: usize = 6;

// ---------------------------------------------------------------- 读表

fn u32_at(off: usize) -> u32 {
    u32::from_le_bytes([TABLE[off], TABLE[off + 1], TABLE[off + 2], TABLE[off + 3]])
}

fn u16_at(off: usize) -> u16 {
    u16::from_le_bytes([TABLE[off], TABLE[off + 1]])
}

/// 读一个 NUL 结尾的字符串。
///
/// 用安全的 `from_utf8` 而不是 `from_utf8_unchecked`：调用点只在按键和渲染上，
/// 一次查找撑死几十次，这点开销换掉一个 `unsafe` 是划算的。
fn cstr(off: usize) -> &'static str {
    let tail = &TABLE[off..];
    let end = tail
        .iter()
        .position(|&b| b == 0)
        .expect("词典里的字符串没有 NUL 结尾");
    std::str::from_utf8(&tail[..end]).expect("词典里有非 UTF-8 字符串")
}

/// 条目总数
pub fn len() -> usize {
    // 表是二进制 blob，索引算错一个字节的表现是「某个拼音查不到」，
    // 不报错、只是静默出错。magic 对不上说明表压根没生成好，早点炸出来。
    debug_assert_eq!(&TABLE[..4], &MAGIC, "词典 magic 不对，ime/pinyin.bin 可能没生成");
    u32_at(8) as usize
}

/// 第 i 条的 key（纯小写 ASCII，表里已按字节序升序排列）
pub fn key_at(i: usize) -> &'static str {
    cstr(u32_at(HEADER_LEN + i * IDX_ENTRY) as usize)
}

fn val_off(i: usize) -> usize {
    u32_at(HEADER_LEN + i * IDX_ENTRY + 4) as usize
}

fn val_n(i: usize) -> usize {
    u16_at(HEADER_LEN + i * IDX_ENTRY + 8) as usize
}

fn syllable_flag(i: usize) -> bool {
    TABLE[HEADER_LEN + i * IDX_ENTRY + 10] & FLAG_SYLLABLE != 0
}

/// 第 i 条的候选。按词频从高到低，最常用的排最前。
pub fn values_at(i: usize) -> impl Iterator<Item = &'static str> {
    struct Values {
        off: usize,
        left: usize,
    }
    impl Iterator for Values {
        type Item = &'static str;
        fn next(&mut self) -> Option<&'static str> {
            if self.left == 0 {
                return None;
            }
            let s = cstr(self.off);
            self.off += s.len() + 1; // NUL 也是 1 字节
            self.left -= 1;
            Some(s)
        }
    }
    Values {
        off: val_off(i),
        left: val_n(i),
    }
}

// ---------------------------------------------------------------- 查找

/// 第一个 `key >= buf` 的下标。二分，不分配。
fn lower_bound(buf: &str) -> usize {
    let (mut lo, mut hi) = (0usize, len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if key_at(mid) < buf {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// 精确命中。
fn exact(buf: &str) -> Option<usize> {
    let i = lower_bound(buf);
    if i < len() && key_at(i) == buf {
        Some(i)
    } else {
        None
    }
}

/// 整串命中：精确优先，否则取第一个以 `buf` 开头的 key（前缀补全）。
///
/// 前缀命中只会落在**紧邻的**那一个 key 上，因为表是按字节序排的 ——
/// `buf` 的所有前缀后继都排在 `lower_bound` 的位置之后。
fn lookup(buf: &str) -> Option<usize> {
    if let Some(i) = exact(buf) {
        return Some(i);
    }
    let i = lower_bound(buf);
    if i < len() && key_at(i).starts_with(buf) {
        Some(i)
    } else {
        None
    }
}

/// `buf` 本身是不是一个合法音节。是的话返回表里那个静态的 key。
fn syllable_key(buf: &str) -> Option<&'static str> {
    let i = exact(buf)?;
    if syllable_flag(i) {
        Some(key_at(i))
    } else {
        None
    }
}

/// 取某个 key 下词频最高的 n 个**单字**候选（分词组合只用单字）。
fn top_chars(key: &str, n: usize) -> Vec<&'static str> {
    match exact(key) {
        Some(i) => values_at(i)
            .filter(|v| v.chars().count() == 1)
            .take(n)
            .collect(),
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------- 分词回退

/// 把 `buf` 切成若干个合法音节，收集最多 `MAX_SEGS` 组切分。
///
/// 长的音节优先试，所以 `yuanshen` 会先给出 `yuan|shen`，再给出 `yu|an|shen`。
fn collect_segs(buf: &str, parts: &mut Vec<&'static str>, out: &mut Vec<Vec<&'static str>>) {
    if out.len() >= MAX_SEGS {
        return;
    }
    if buf.is_empty() {
        // 只收「切成两段及以上」的 —— 整串就是一个音节的情况，第 1 步已经处理过了
        if parts.len() >= 2 {
            out.push(parts.clone());
        }
        return;
    }
    if parts.len() >= MAX_PARTS {
        return;
    }

    let max = buf.len().min(SYLLABLE_MAX_LEN);
    for n in (1..=max).rev() {
        if !buf.is_char_boundary(n) {
            continue;
        }
        let (head, rest) = buf.split_at(n);
        if let Some(key) = syllable_key(head) {
            parts.push(key);
            collect_segs(rest, parts, out);
            parts.pop();
        }
    }
}

/// 把一组切分里各段的候选拼成词，按「各段词频名次之和」由低到高。
///
/// 用里程表式的进位枚举，天然就是这个顺序：
/// `(0,0) (0,1) (0,2) (1,0) (1,1) …`，也就是最常用的字打头先出。
fn push_combos(seg: &[&'static str], out: &mut Vec<String>) {
    let tops: Vec<Vec<&'static str>> = seg.iter().map(|k| top_chars(k, COMBO_PER_PART)).collect();
    if tops.iter().any(|t| t.is_empty()) {
        return;
    }

    let n = tops.len();
    let mut idx = vec![0usize; n];
    'odometer: loop {
        let mut word = String::new();
        for p in 0..n {
            word.push_str(tops[p][idx[p]]);
        }
        if !out.contains(&word) {
            out.push(word);
            if out.len() >= MAX_CANDS {
                return;
            }
        }

        // 从最低位进位
        let mut p = n;
        loop {
            if p == 0 {
                break 'odometer; // 全部进位溢出 = 枚举完了
            }
            p -= 1;
            idx[p] += 1;
            if idx[p] < tops[p].len() {
                break;
            }
            idx[p] = 0;
        }
    }
}

// ---------------------------------------------------------------- 输入状态

/// 拼音输入缓冲和它当前的候选。
///
/// 和「已经上屏的文字」是分开的：这里的 `buf` 还没进输入框，
/// 用户点候选或者按空格才落定。
#[derive(Default)]
pub struct Compose {
    /// 已输入的拼音字母，如 `yuan`
    buf: String,
    /// 当前候选（整串命中的 + 分词组合的）
    cands: Vec<String>,
    /// 选中的候选在 `cands` 里的下标
    sel: usize,
}

impl Compose {
    pub fn new() -> Self {
        Self::default()
    }

    /// 拼音缓冲，如 `yuan`。空串表示还没开始拼。
    pub fn buf(&self) -> &str {
        &self.buf
    }

    /// 是否正在拼字（缓冲非空）
    pub fn composing(&self) -> bool {
        !self.buf.is_empty()
    }

    pub fn cands(&self) -> &[String] {
        &self.cands
    }

    /// 敲一个字母。非 ASCII 小写字母直接忽略 —— 数字和符号由调用方另走一条路。
    pub fn push(&mut self, c: char) {
        if !c.is_ascii_lowercase() {
            return;
        }
        self.buf.push(c);
        self.refresh();
    }

    /// 退格。返回 true 表示吃掉了缓冲里的一个字母；
    /// false 表示缓冲已经空了，调用方应该去删输入框里已经上屏的字。
    pub fn backspace(&mut self) -> bool {
        if self.buf.pop().is_none() {
            return false;
        }
        self.refresh();
        true
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.cands.clear();
        self.sel = 0;
    }

    /// 取走当前选中的候选并清空缓冲 —— 这就是「上屏」。
    pub fn take(&mut self) -> Option<String> {
        let picked = self.cands.get(self.sel).cloned();
        self.clear();
        picked
    }

    /// 取走整个拼音缓冲**原样**（当英文字母用）并清空。
    ///
    /// 这是「用户没挑候选」时的落定方式：打了 `naruto` 又没点候选就按确定，
    /// 那串字母**本身就是他要搜的东西** —— 不该被换成别的字，也不该丢掉。
    ///
    /// 和 `take()` 的区别就是这条：`take()` 落的是选中的候选（中文），
    /// 这里落的是缓冲里那串字母（英文）。见 `main.rs` 的 `raw_commit_text`。
    pub fn take_raw(&mut self) -> String {
        self.cands.clear();
        self.sel = 0;
        std::mem::take(&mut self.buf)
    }

    // ------------------------------------------------------------ 候选栏

    pub fn page(&self) -> usize {
        self.sel / PAGE
    }

    pub fn page_count(&self) -> usize {
        self.cands.len().div_ceil(PAGE)
    }

    /// 选中项在本页内的下标，给 UI 高亮用。
    pub fn sel_in_page(&self) -> usize {
        self.sel % PAGE
    }

    /// 本页要显示的候选。
    pub fn page_items(&self) -> &[String] {
        let start = self.page() * PAGE;
        let end = (start + PAGE).min(self.cands.len());
        &self.cands[start..end]
    }

    /// 翻页。`delta` 是页数。到头/到尾就停住，不循环 ——
    /// 循环会让「翻到头了」这件事看不出来。
    pub fn turn_page(&mut self, delta: i32) {
        let pages = self.page_count() as i32;
        if pages == 0 {
            return;
        }
        let target = (self.page() as i32 + delta).clamp(0, pages - 1) as usize;
        self.sel = target * PAGE;
    }

    /// 点候选栏里的第 i 个（本页内下标）。返回是否命中。
    pub fn pick_in_page(&mut self, i: usize) -> bool {
        let global = self.page() * PAGE + i;
        if global < self.cands.len() {
            self.sel = global;
            true
        } else {
            false
        }
    }

    // ------------------------------------------------------------ 候选计算

    fn refresh(&mut self) {
        self.cands.clear();
        self.sel = 0;
        if self.buf.is_empty() {
            return;
        }

        // 1) 整串命中（精确 → 前缀补全）
        let mut whole_is_syllable = false;
        if let Some(i) = lookup(&self.buf) {
            whole_is_syllable = syllable_flag(i);
            self.cands
                .extend(values_at(i).take(MAX_CANDS).map(str::to_string));
        }

        // 2) 分词回退。整串命中之后**仍然要拼**：
        //    `yuanshen` 整词给的是「元神／原审」，用户要的可能是「原」+「神」。
        //
        //    但整串本身就是一个音节时不做 —— `yuan` 既是整串命中（44 个单字），
        //    又能切成 `yu`+`an`，拼出来的是「于安／与安」这种噪音，
        //    只会把真正的候选挤到后面去。用户打一个音节就是一个音节。
        if whole_is_syllable {
            return;
        }
        let mut segs: Vec<Vec<&'static str>> = Vec::new();
        let mut parts: Vec<&'static str> = Vec::new();
        collect_segs(&self.buf, &mut parts, &mut segs);
        segs.sort_by_key(|p| p.len()); // 段数少的优先：整词比拆碎的更像答案
        for seg in segs.iter().take(MAX_SEGS) {
            if self.cands.len() >= MAX_CANDS {
                break;
            }
            push_combos(seg, &mut self.cands);
        }
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 把整个表扫一遍，确认结构没被破坏。
    ///
    /// 这一条是下面所有测试的地基：表是二进制 blob，索引算错一个字节
    /// 表现可能就是「某个拼音查不到」或者「候选里混进别的字」，
    /// 不报错、只是静默错。
    #[test]
    fn 表结构完整() {
        assert_eq!(&TABLE[..4], &MAGIC, "magic 不对，表可能没生成");
        let n = len();
        assert!(n > 30_000, "条目数只有 {n}，表看着不完整");
        assert!(TABLE.len() > 1_000_000, "表只有 {} 字节", TABLE.len());
    }

    #[test]
    fn key_升序且是纯小写字母() {
        let n = len();
        for i in 0..n {
            let k = key_at(i);
            assert!(!k.is_empty(), "第 {i} 条的 key 是空的");
            assert!(
                k.bytes().all(|b| b.is_ascii_lowercase()),
                "第 {i} 条的 key {k:?} 不是纯小写字母"
            );
            if i > 0 {
                assert!(key_at(i - 1) < k, "第 {i} 条起 key 不再升序：{k:?}");
            }
        }
    }

    #[test]
    fn 候选非空且是合法_utf8() {
        let mut total = 0usize;
        for i in 0..len() {
            let vs: Vec<&str> = values_at(i).collect();
            assert_eq!(vs.len(), val_n(i), "第 {i} 条候选数对不上");
            assert!(!vs.is_empty(), "第 {i} 条（{}）一个候选都没有", key_at(i));
            for v in vs {
                assert!(!v.is_empty(), "第 {i} 条里有空候选");
                assert!(
                    !v.chars().any(|c| (c as u32) < 0x20),
                    "候选 {v:?} 里有控制字符"
                );
                total += 1;
            }
        }
        assert!(total > 50_000, "候选总数只有 {total}");
    }

    #[test]
    fn 音节标志位只打在音节上() {
        let sylls: Vec<&str> = (0..len())
            .filter(|&i| syllable_flag(i))
            .map(key_at)
            .collect();
        assert!(
            (350..=450).contains(&sylls.len()),
            "音节数 {} 不在合理区间，判别规则可能失效了",
            sylls.len()
        );
        // 真音节
        for k in ["yuan", "shen", "xian", "ju", "zhuang", "a"] {
            assert!(sylls.contains(&k), "{k} 应该是音节");
        }
        // 词组不能被当成音节，否则 `baba` 会被整段吃掉，切不出 ba|ba
        for k in ["baba", "badao", "zhongwen", "shurufa", "kindle"] {
            assert!(!sylls.contains(&k), "{k} 不该被当成音节");
        }
    }

    #[test]
    fn 精确查找_同音字按词频排序() {
        let i = exact("yuan").expect("查不到 yuan");
        let vs: Vec<&str> = values_at(i).take(6).collect();
        assert_eq!(vs, ["元", "远", "原", "源", "员", "园"]);

        let i = exact("dao").expect("查不到 dao");
        assert_eq!(values_at(i).next(), Some("到"));
    }

    #[test]
    fn 前缀补全_打一半就出结果() {
        // `eryang` 本身不是词，但表里有 `eryanghualiu`
        assert!(exact("eryang").is_none());
        let i = lookup("eryang").expect("前缀补全失败");
        assert_eq!(key_at(i), "eryanghualiu");
        assert_eq!(values_at(i).next(), Some("二氧化硫"));
    }

    #[test]
    fn 查不到就返回_none() {
        for buf in ["zzz", "kindle", "qqqqqqq", "vvvv"] {
            assert!(lookup(buf).is_none(), "{buf} 不该查到东西");
        }
    }

    #[test]
    fn 输入与退格() {
        let mut c = Compose::new();
        assert!(!c.composing());
        for ch in "yuan".chars() {
            c.push(ch);
        }
        assert_eq!(c.buf(), "yuan");
        assert!(c.composing());
        assert_eq!(c.cands().first().map(String::as_str), Some("元"));

        // 退格回到空
        for _ in 0..4 {
            assert!(c.backspace());
        }
        assert!(!c.composing());
        assert!(c.cands().is_empty());
        // 已经空了，再退格要交给调用方去删输入框
        assert!(!c.backspace());
    }

    #[test]
    fn 非字母不进食() {
        let mut c = Compose::new();
        for ch in ['1', 'A', ' ', '-', '中'] {
            c.push(ch);
        }
        assert!(!c.composing(), "缓冲里混进了非小写字母");
    }

    #[test]
    fn 上屏拿走选中的候选() {
        let mut c = Compose::new();
        for ch in "dao".chars() {
            c.push(ch);
        }
        assert_eq!(c.take().as_deref(), Some("到"));
        assert!(!c.composing(), "上屏之后缓冲要清干净");
        assert!(c.cands().is_empty());

        // 换一个候选
        for ch in "dao".chars() {
            c.push(ch);
        }
        assert!(c.pick_in_page(3));
        assert_eq!(c.take().as_deref(), Some("刀"));
    }

    /// 没挑候选时，缓冲要能被**原样**取走 —— 那串字母就是用户要搜的英文。
    ///
    /// 这条是「拼音模式下搜不了英文」那个 bug 的地基测试：以前确定走的是
    /// `take()`，而 `take()` 永远返回第一个候选，于是 `naruto`（表里没这个音）
    /// 落出个 `None` 什么都不上屏，`one`（恰好是合法拼音）落出「哦呢」。
    #[test]
    fn 原样取走缓冲() {
        let mut c = Compose::new();
        for ch in "naruto".chars() {
            c.push(ch);
        }
        assert!(c.cands().is_empty(), "naruto 不该有候选");
        assert_eq!(c.take_raw(), "naruto");
        assert!(!c.composing(), "取走之后缓冲要清干净");
        assert!(c.cands().is_empty());

        // 有候选时也一样：没点候选就是没要中文，原样给回去
        let mut c = Compose::new();
        for ch in "one".chars() {
            c.push(ch);
        }
        assert!(!c.cands().is_empty(), "one 是合法拼音，应该有候选");
        assert_eq!(c.take_raw(), "one", "有候选也不该擅自把第一个候选顶上来");

        // 空缓冲取走是空串，不是 panic
        let mut c = Compose::new();
        assert_eq!(c.take_raw(), "");
    }

    /// 这条是这个模块存在的理由：整词命中给的候选不一定是用户想要的，
    /// 分词回退要能把「原」+「神」拼出来。
    #[test]
    fn 分词回退能拼出原神() {
        let mut c = Compose::new();
        for ch in "yuanshen".chars() {
            c.push(ch);
        }
        // 整词命中的在前
        assert_eq!(c.cands().first().map(String::as_str), Some("元神"));
        assert!(
            c.cands().iter().any(|s| s == "原神"),
            "候选里没有「原神」：{:?}",
            &c.cands()[..c.cands().len().min(20)]
        );
        // 「原神」要能被点到（不能掉到 MAX_CANDS 之外）
        let at = c.cands().iter().position(|s| s == "原神").unwrap();
        assert!(at < MAX_CANDS, "「原神」排在 {at}，超出了候选上限");

        let page = at / PAGE;
        c.turn_page(0);
        for _ in 0..page {
            c.turn_page(1);
        }
        assert!(c.pick_in_page(at % PAGE));
        assert_eq!(c.take().as_deref(), Some("原神"));
    }

    /// 整串就是一个音节的输入，分词不能掺进来。
    ///
    /// `yuan` 既能整串命中（44 个候选），又能切成 `yu`+`an` 两个合法音节。
    /// 要是分词不设限制，候选里就会混进「于安／与安」这类噪音 ——
    /// 用户打的是一个音节，不该看到两字词。
    #[test]
    fn 单音节输入不掺分词噪音() {
        let mut c = Compose::new();
        for ch in "yuan".chars() {
            c.push(ch);
        }
        assert_eq!(c.cands().len(), 44, "yuan 的候选数变了");
        assert_eq!(c.cands().first().map(String::as_str), Some("元"));
        assert!(
            !c.cands().iter().any(|s| s == "于安"),
            "分词噪音混进了单音节输入的候选里"
        );
    }

    /// 反过来的一面：整串不是单个音节（是个词）时，分词必须生效 ——
    /// 这正是上面那条限制的反例，两条合起来才算把规则钉住。
    #[test]
    fn 整词输入仍然给分词候选() {
        let mut c = Compose::new();
        for ch in "zhongwen".chars() {
            c.push(ch);
        }
        // 「中文」是整词命中
        assert_eq!(c.cands().first().map(String::as_str), Some("中文"));
        // zhong|wen 也能切，所以「重」「文」的其它组合要出现
        assert!(
            c.cands().iter().filter(|s| s.chars().count() == 2).count() > 1,
            "整词输入没有产生分词候选：{:?}",
            c.cands()
        );
    }

    #[test]
    fn 翻页夹在边界内() {
        let mut c = Compose::new();
        for ch in "yi".chars() {
            c.push(ch);
        }
        let pages = c.page_count();
        assert!(pages > 1, "yi 应该有很多候选，实际 {pages} 页");

        // 往前翻到头不该动
        c.turn_page(-1);
        assert_eq!(c.page(), 0);
        // 往后翻到头也不该越界
        for _ in 0..pages + 5 {
            c.turn_page(1);
        }
        assert_eq!(c.page(), pages - 1);
        // 最后一页的候选数是对的
        assert!(!c.page_items().is_empty());
        assert!(c.page_items().len() <= PAGE);
    }

    /// 点到没有候选的格子（本页没排满时的空格）不该改变选中。
    ///
    /// 这条**故意手工摆状态**，不靠某个音节的候选数 ——
    /// 原来用的是 `a`，假设它不到一页；`PAGE` 从 12 改成 5 之后这个假设就不成立了。
    /// 候选数是数据决定的，测试不该依赖它。测试和实现同模块，私有字段直接摆。
    #[test]
    fn 点空白候选格不生效() {
        let mut c = Compose::new();
        c.buf = "x".into();
        c.cands = vec!["甲".into(), "乙".into()];
        c.sel = 0;

        assert!(!c.pick_in_page(2), "第 2 格是空的，点了不该生效");
        assert!(!c.pick_in_page(PAGE - 1), "最后一格也是空的");
        assert_eq!(c.sel, 0, "点空格不该改变选中");

        assert!(c.pick_in_page(1), "第 1 格有候选，该生效");
        assert_eq!(c.sel, 1);

        // 一个候选都没有时，任何格子都不该生效
        c.cands.clear();
        assert!(!c.pick_in_page(0));
    }
}

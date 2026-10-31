//! 收藏（**先做本地版**）。
//!
//! 用户拍板：收藏先存在本地文件里，以后他自己去给服务端加接口再换。
//! 所以这一层刻意做成「可替换」的样子：
//!
//!   * 纯逻辑（解析 / 序列化 / 去重 / 上限 / 增删查）都是不碰 IO 的自由函数，
//!     有单元测试钉住 —— 收藏这种「写错了不报错、只是下次打开发现没了」的东西，
//!     最值得用测试钉；
//!   * IO 收在一个 [`Backend`] trait 后面，现在只有 [`LocalFile`] 一种实现。
//!     以后要接服务端，写一个 `HttpBackend`（GET/POST /api/favorites）实现同一个
//!     trait、在 main.rs 里把构造函数换掉即可，上层（Worker / 界面）一行都不用动。
//!
//! 文件放在**程序所在目录** `favorites.json`，和 `server.txt` / `refresh.txt`
//! 同一套路（定位程序目录见 main.rs 的 `config_path_named`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 文件名常量。和 server.txt / refresh.txt 一样落在程序目录。
pub const FILE: &str = "favorites.json";

/// 收藏条数上限。
///
/// 用户偏好「另加不删减」，所以上限给得宽松（200 条够堆很久）；
/// 但也不能无限堆 —— Kindle 的闪存慢，文件大了每次读写都卡。
/// 超过上限时丢**最早**加的那条（数组头部），新加的永远留着。
pub const MAX: usize = 200;

/// 收藏类型：本子。
pub const KIND_ALBUM: &str = "album";
/// 收藏类型：单话。
pub const KIND_CHAPTER: &str = "chapter";

/// 一条收藏。
///
/// 字段刻意保持「服务端也能存」的形状：kind + id 是主键，title 是为了列表
/// 显示（服务端模式下也可以只用 id 再去查，但存着能省一次往返）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fav {
    pub kind: String,
    pub id: String,
    pub title: String,
    /// 单话所属本子的 id。收藏本子时是空串。
    #[serde(default)]
    pub album_id: String,
    /// 单话所属**本子**的标题。收藏本子时是空串。
    ///
    /// 为什么要单独存：收藏页的单话行要显示「本子标题 / 第 N 话」两行，
    /// 而单话自己的 `title` 只是「第 1 話」这种短标签，**看不出是哪个本子**。
    /// 光靠 `album_id` 得回服务端查一次（列表里八行就是八次往返）。
    ///
    /// ⚠️ `#[serde(default)]` 不能省：真机上已有的 `favorites.json` 里
    /// 没有这个字段，缺了它整个文件会解析失败 —— 而按「坏数据当空表」的
    /// 约定，表现就是**用户已有的收藏全都不见了**。
    #[serde(default)]
    pub album_title: String,
}

impl Fav {
    pub fn album(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: KIND_ALBUM.into(),
            id: id.into(),
            title: title.into(),
            album_id: String::new(),
            album_title: String::new(),
        }
    }

    /// 收藏一话。
    ///
    /// `title` 是这一话自己的短标签（如「第 1 話」），`album_title` 是所属本子的
    /// 标题 —— 列表上两行都要用，见 [`Fav::album_title`] 的说明。
    pub fn chapter(
        id: impl Into<String>,
        title: impl Into<String>,
        album_id: impl Into<String>,
        album_title: impl Into<String>,
    ) -> Self {
        Self {
            kind: KIND_CHAPTER.into(),
            id: id.into(),
            title: title.into(),
            album_id: album_id.into(),
            album_title: album_title.into(),
        }
    }

    /// 主键。`kind` 和 `id` 拼起来 —— 本子 id 和单话 id 是两个命名空间，
    /// 只看 id 会把它们混成一条。
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.id)
    }

    pub fn is_album(&self) -> bool {
        self.kind == KIND_ALBUM
    }
}

// ---------------------------------------------------------------- 纯逻辑

/// 从 JSON 文本解析。**坏数据一律当成空表**，绝不 panic。
///
/// 这条很重要：用户可能手改过文件，也可能写到一半断电把文件截断了。
/// 解析失败时宁可「这一次显示成没有收藏」，也不能让整个应用起不来。
///
/// 顶层兼容两种形状：裸数组 `[…]`，以及带一层的 `{"items":[…]}`（方便以后
/// 服务端返回时外面套一层元信息）。
pub fn parse(text: &str) -> Vec<Fav> {
    #[derive(Deserialize)]
    struct Wrapper {
        items: Vec<Fav>,
    }

    if let Ok(v) = serde_json::from_str::<Vec<Fav>>(text) {
        return dedup(v);
    }
    if let Ok(w) = serde_json::from_str::<Wrapper>(text) {
        return dedup(w.items);
    }
    Vec::new()
}

/// 序列化成 JSON。**顶层写裸数组** —— 我们自己的读端两种都认，但对外暴露的
/// 形状越简单越好，将来手改也方便。
pub fn serialize(items: &[Fav]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

/// 按键去重：同一个 `(kind,id)` 只留**最后一条**（后加的覆盖先加的）。
///
/// 用「先删旧的再 push」而不是 HashSet：文件很小，保序更重要 ——
/// 列表顺序就是收藏顺序，重排会让用户以为收藏丢了。
pub fn dedup(items: Vec<Fav>) -> Vec<Fav> {
    let mut out: Vec<Fav> = Vec::new();
    for f in items {
        if let Some(pos) = out.iter().position(|x| x.key() == f.key()) {
            out.remove(pos);
        }
        out.push(f);
    }
    out
}

/// 加一条（去重 + 限长）。
///
/// 已存在就删掉旧的、追加到末尾 —— 顺序即最近收藏顺序。
pub fn upsert(items: &mut Vec<Fav>, fav: Fav) {
    if let Some(pos) = items.iter().position(|x| x.key() == fav.key()) {
        items.remove(pos);
    }
    items.push(fav);
    // 超上限丢最早的。倒着删是因为每删一个下标会变，从头删最省心。
    while items.len() > MAX {
        items.remove(0);
    }
}

/// 删一条。不存在时静默什么都不做（取消收藏一个本来就没收藏的东西不算错）。
pub fn remove(items: &mut Vec<Fav>, kind: &str, id: &str) {
    items.retain(|f| !(f.kind == kind && f.id == id));
}

pub fn contains(items: &[Fav], kind: &str, id: &str) -> bool {
    items.iter().any(|f| f.kind == kind && f.id == id)
}

// ---------------------------------------------------------------- 持久化

/// 收藏的持久化后端。现在只有本地文件一种实现；换服务端时实现同一个 trait。
pub trait Backend {
    fn load(&self) -> Vec<Fav>;
    fn store(&self, items: &[Fav]) -> Result<(), String>;
}

/// 本地文件实现。
///
/// `path` 为 `None`（定位不到程序目录）时退化成「内存模式」：能读能写、只是
/// 不落盘。这样即使程序目录不可写，收藏按钮也不会点了没反应。
#[derive(Default)]
pub struct LocalFile {
    pub path: Option<PathBuf>,
}

impl Backend for LocalFile {
    fn load(&self) -> Vec<Fav> {
        match self.path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(text) => parse(&text),
            None => Vec::new(),
        }
    }

    fn store(&self, items: &[Fav]) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(()); // 内存模式：没有落盘目标，不算失败
        };
        std::fs::write(path, serialize(items))
            .map_err(|e| format!("写入 {} 失败：{e}", path.display()))
    }
}

/// 收藏表 = 内存里的一份列表 + 一个后端。
pub struct Favorites {
    backend: Box<dyn Backend>,
    items: Vec<Fav>,
    /// 内存里的列表和磁盘不一致。只有真改过才置位，`save()` 据此跳过
    /// 没必要的写入 —— 闪存擦写是有寿命的。
    dirty: bool,
}

impl Favorites {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        let items = backend.load();
        Self {
            backend,
            items,
            dirty: false,
        }
    }

    /// 本地文件版。`path` 由调用方给（main.rs 用它定位程序目录）。
    pub fn local(path: Option<PathBuf>) -> Self {
        Self::new(Box::new(LocalFile { path }))
    }

    /// 模块对外的小 API：main 目前只用得到一部分，但 `all` / `contains`
    /// 是「本地版 -> 服务端版」这层抽象该有的读接口，留着。
    #[allow(dead_code)]
    pub fn all(&self) -> &[Fav] {
        &self.items
    }

    /// 收藏的作品，按加入顺序。
    pub fn albums(&self) -> Vec<Fav> {
        self.items.iter().filter(|f| f.is_album()).cloned().collect()
    }

    /// 收藏的单话。
    pub fn chapters(&self) -> Vec<Fav> {
        self.items
            .iter()
            .filter(|f| !f.is_album())
            .cloned()
            .collect()
    }

    #[allow(dead_code)]
    pub fn contains(&self, kind: &str, id: &str) -> bool {
        contains(&self.items, kind, id)
    }

    pub fn contains_album(&self, id: &str) -> bool {
        contains(&self.items, KIND_ALBUM, id)
    }

    pub fn contains_chapter(&self, id: &str) -> bool {
        contains(&self.items, KIND_CHAPTER, id)
    }

    /// 加/取消收藏。返回**操作后是否已收藏**（true = 现在在收藏里）。
    pub fn toggle(&mut self, fav: Fav) -> bool {
        let now;
        if contains(&self.items, &fav.kind, &fav.id) {
            remove(&mut self.items, &fav.kind, &fav.id);
            now = false;
        } else {
            upsert(&mut self.items, fav);
            now = true;
        }
        self.dirty = true;
        now
    }

    /// 落盘。没改过就什么都不做（省一次闪存写入）。
    pub fn save(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.backend.store(&self.items)?;
        self.dirty = false;
        Ok(())
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 内存后端：不碰磁盘，用来测 Favorites 的增删逻辑。
    #[derive(Default)]
    struct MemBackend {
        saved: std::cell::RefCell<Vec<Fav>>,
    }
    impl Backend for MemBackend {
        fn load(&self) -> Vec<Fav> {
            self.saved.borrow().clone()
        }
        fn store(&self, items: &[Fav]) -> Result<(), String> {
            *self.saved.borrow_mut() = items.to_vec();
            Ok(())
        }
    }

    fn 一条(i: usize) -> Fav {
        Fav::album(format!("A{i}"), format!("作品{i}"))
    }

    /// 写进去再读出来，必须一模一样 —— 这是收藏功能的地基。
    #[test]
    fn 读写往返() {
        let dir = std::env::temp_dir().join(format!(
            "km-fav-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("favorites.json");

        let mut f = Favorites::local(Some(path.clone()));
        assert!(f.all().is_empty(), "新文件该是空的");
        f.toggle(Fav::album("100", "某本子"));
        f.toggle(Fav::chapter("200", "第 1 话", "100", "某本子"));
        f.toggle(Fav::album("100", "某本子")); // 再点一次 = 取消
        f.save().unwrap();

        let reloaded = Favorites::local(Some(path.clone()));
        assert_eq!(reloaded.all().len(), 1, "只有那条单话该留下");
        assert!(reloaded.contains_chapter("200"));
        assert!(!reloaded.contains_album("100"), "取消收藏没生效");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 损坏的 JSON 不能让应用崩，也不能让收藏变成别的东西 —— 当空表。
    #[test]
    fn 损坏的_json_不panic() {
        for bad in ["", "   ", "{", "not json", "{\"items\": 3}", "[{\"kind\":\"album\"}]"] {
            assert!(parse(bad).is_empty(), "{bad:?} 应该解析成空表");
        }
    }

    /// 裸数组和带一层 items 两种形状都要认。
    #[test]
    fn 两种顶层形状都认() {
        let one = serialize(&[Fav::album("1", "甲")]);
        assert_eq!(parse(&one).len(), 1, "裸数组");
        assert_eq!(parse(&format!("{{\"items\":{one}}}")).len(), 1, "带 items 外层");
    }

    /// 同一个 (kind,id) 去重，后加的覆盖先加的，且保持顺序。
    #[test]
    fn 去重() {
        let items = vec![
            Fav::album("1", "旧标题"),
            Fav::album("2", "乙"),
            Fav::album("1", "新标题"),
        ];
        let out = dedup(items);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "乙", "先出现的那条应该留在原位");
        assert_eq!(out[1].title, "新标题", "后加的覆盖标题");
    }

    /// upsert 重复加同一条不会变长。
    #[test]
    fn upsert_不重复() {
        let mut items = Vec::new();
        upsert(&mut items, Fav::album("1", "甲"));
        upsert(&mut items, Fav::album("1", "甲改"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "甲改");
    }

    /// 超过上限丢最早的，新加的必须留着。
    #[test]
    fn 超上限丢最早的() {
        let mut items = Vec::new();
        for i in 0..MAX + 5 {
            upsert(&mut items, 一条(i));
        }
        assert_eq!(items.len(), MAX, "没被限长");
        assert!(
            !contains(&items, KIND_ALBUM, "A0"),
            "最早那条该被丢掉"
        );
        let last = MAX + 4;
        assert!(
            contains(&items, KIND_ALBUM, &format!("A{last}")),
            "最新那条不该被丢"
        );
    }

    /// toggle 的往返：加 -> 在，再点 -> 不在。
    #[test]
    fn toggle_往返() {
        let mut f = Favorites::new(Box::<MemBackend>::default());
        assert!(f.toggle(Fav::album("7", "七")), "第一次点该是收藏");
        assert!(f.contains_album("7"));
        assert!(!f.toggle(Fav::album("7", "七")), "第二次点该是取消");
        assert!(!f.contains_album("7"));
    }

    /// 没改过就不写盘（dirty 门）。用内存后端数一下 store 被调了几次。
    #[test]
    fn 没改过不写盘() {
        let backend = Box::new(MemBackend::default());
        let mut f = Favorites::new(backend);
        // 先加一条并落盘
        f.toggle(Fav::album("1", "甲"));
        f.save().unwrap();
        // 再存一次：没改过，不该报错也不该多做事情
        f.save().unwrap();
        assert_eq!(f.all().len(), 1);
    }

    /// 按类型分栏：albums / chapters 各归各。
    #[test]
    fn 分栏() {
        let mut f = Favorites::new(Box::<MemBackend>::default());
        f.toggle(Fav::album("1", "本"));
        f.toggle(Fav::chapter("2", "话", "1", "本"));
        assert_eq!(f.albums().len(), 1);
        assert_eq!(f.chapters().len(), 1);
        assert_eq!(f.albums()[0].id, "1");
        assert_eq!(f.chapters()[0].album_id, "1");
    }

    static NEXT: AtomicU32 = AtomicU32::new(0);
}

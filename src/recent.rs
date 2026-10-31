//! 最近搜索（**本地文件版**）。
//!
//! 用户要求「搜索页能点回上次搜的词」。存本地最省事：不用改服务端接口，
//! 断网时也还在。文件放在**程序所在目录** `search_history.json`，
//! 和 `favorites.json` / `server.txt` 同一套路（定位见 main.rs 的
//! `config_path_named`）。
//!
//! 纯逻辑（解析 / 序列化 / 去重 / 限长）都是不碰 IO 的自由函数，有单元测试
//! 钉住 —— 这类数据写错了不会报错，只会「下次打开发现词没了」。
//!
//! ⚠️ 文件读不出来、内容损坏时一律当空表，绝不 panic（理由同 favorites.rs）。

use std::path::PathBuf;

/// 文件名常量。落在程序目录。
pub const FILE: &str = "search_history.json";

/// 最多记多少条。
///
/// 10 条足够翻回最近几次；Kindle 闪存慢，文件别无限涨。
/// 超上限时丢**最早**那条（数组尾部），新搜的永远留着。
pub const MAX: usize = 10;

/// 从 JSON 文本解析。坏数据 / 空串一律当空表，绝不 panic。
///
/// 顺手做了三件事：去掉首尾空白、丢掉空词、按现有顺序去重。
pub fn parse(text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Vec<String>>(text) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for s in v {
        let s = s.trim().to_string();
        if s.is_empty() || out.contains(&s) {
            continue;
        }
        out.push(s);
        if out.len() >= MAX {
            break;
        }
    }
    out
}

/// 序列化成 JSON（顶层裸数组，手改也方便）。
pub fn serialize(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".into())
}

/// 记一条关键词：已存在就**挪到最前**，超上限丢最旧的。
///
/// 顺序即「最近搜索」顺序（第 0 条最新）。
pub fn add(items: &mut Vec<String>, keyword: &str) {
    let k = keyword.trim();
    if k.is_empty() {
        return;
    }
    if let Some(pos) = items.iter().position(|x| x == k) {
        items.remove(pos);
    }
    items.insert(0, k.to_string());
    items.truncate(MAX);
}

/// 最近搜索表 = 内存里的一份列表 + 落盘路径。
///
/// 改动很小、文件也很小，所以每次增删都直接落盘（不像收藏那样攒到退出）。
pub struct Recent {
    path: Option<PathBuf>,
    items: Vec<String>,
}

impl Recent {
    /// `path` 为 `None`（定位不到程序目录）时退化成「内存模式」：
    /// 能读能写、只是不落盘。这样最近搜索不会点了没反应。
    pub fn local(path: Option<PathBuf>) -> Self {
        let items = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| parse(&t))
            .unwrap_or_default();
        Self { path, items }
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }

    pub fn add(&mut self, keyword: &str) {
        let before = self.items.clone();
        add(&mut self.items, keyword);
        // 没变化就不写盘 —— 闪存擦写是有寿命的
        if before != self.items {
            self.save();
        }
    }

    pub fn clear(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.items.clear();
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return; // 内存模式：没有落盘目标，不算失败
        };
        if let Err(e) = std::fs::write(path, serialize(&self.items)) {
            log::warn!("写最近搜索失败：{e}");
        }
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 重复的词挪到最前() {
        let mut v = Vec::new();
        add(&mut v, "a");
        add(&mut v, "b");
        add(&mut v, "a");
        assert_eq!(v, vec!["a", "b"], "重复词该挪到最前，不该出现两条");
    }

    #[test]
    fn 超上限丢最旧的() {
        let mut v = Vec::new();
        for i in 0..MAX + 3 {
            add(&mut v, &format!("k{i}"));
        }
        assert_eq!(v.len(), MAX, "没被限长");
        assert_eq!(v[0], format!("k{}", MAX + 2), "最新的该在最前");
        assert!(!v.contains(&"k0".to_string()), "最早那条该被丢掉");
    }

    #[test]
    fn 空词不记() {
        let mut v = Vec::new();
        add(&mut v, "");
        add(&mut v, "   ");
        assert!(v.is_empty(), "空串/纯空白不该进列表");
    }

    #[test]
    fn 两边空白会去掉() {
        let mut v = Vec::new();
        add(&mut v, "  北京  ");
        assert_eq!(v, vec!["北京"]);
    }

    #[test]
    fn 损坏的_json_当空表() {
        for bad in ["", "   ", "{", "not json", "{\"items\":3}", "[1,2,3]"] {
            assert!(parse(bad).is_empty(), "{bad:?} 应该解析成空表");
        }
    }

    #[test]
    fn 序列化再解析() {
        let v = vec!["甲".to_string(), "b".to_string()];
        assert_eq!(parse(&serialize(&v)), v);
    }
}

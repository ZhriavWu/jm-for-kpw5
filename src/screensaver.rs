//! 屏保：挂起前把用户的屏保图铺到屏幕上。
//!
//! # 为什么需要它
//!
//! Kindle 的正常流程是 **powerd 先画出屏保，再挂起**。但这个动作要穿过框架
//! （`blanket` 那个 X 程序负责画屏保窗口），而 `launch.sh` 把框架冻住了 ——
//! 于是我们接管休眠之后，**屏幕上就停在最后看的那一页漫画上**，没有屏保。
//!
//! 所以这一步得自己补上：挂起前把屏保图画出来。
//!
//! # 怎么画
//!
//! 不用自己去碰 `/dev/fb0` 和 EPDC 的 ioctl —— 那套东西 Slint 后端已经处理好了，
//! 应用本来也带 PNG 解码（给漫画用的）。所以路线是：
//!
//! ```text
//! 读 /mnt/us/screensaver/bg_ss*.png
//!   → 解码成像素缓冲（就是漫画用的那条路）
//!   → 交给 Slint 当一张全屏图显示
//!   → Slint 后端负责写帧缓冲 + 触发 EPDC 刷新
//! ```
//!
//! 屏保图是 **1236×1648**，正好等于面板尺寸，所以是 1:1 直接贴上去，
//! 不缩放、不变形。
//!
//! # 关于多张图
//!
//! 系统支持 `bg_ss00.png`、`bg_ss01.png` … 并会轮换。这里也照做：
//! 扫出目录里所有 `bg_ss*.png`，每次挂起换下一张。
//! 目录为空或图读不出来时返回 `None`，调用方就按「没有屏保」处理 ——
//! 挂起照常进行，只是屏幕上留着当前画面。

// 宿主上跑测试时这个模块也会编进来，但 next_image 在那时用不到
#![cfg_attr(not(all(feature = "kindle", target_arch = "arm")), allow(dead_code))]

use std::path::PathBuf;
use std::sync::Mutex;

use crate::render::{self, PixelBuffer};

/// 系统屏保目录。用户直接替换这里的图片来换屏保，
/// 所以**不要去别的地方找**，就用系统那一套。
const SS_DIR: &str = "/mnt/us/screensaver";

/// 屏保目录里的图片，以及轮换到第几张。
struct Cache {
    paths: Vec<PathBuf>,
    next: usize,
    /// 已解码的图，按需加载后缓存。
    /// `None` 表示「试过了但这个文件读不出来」，免得每次挂起都重试一遍坏文件。
    loaded: Vec<Option<PixelBuffer>>,
    /// 扫目录只做一次
    scanned: bool,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    paths: Vec::new(),
    next: 0,
    loaded: Vec::new(),
    scanned: false,
});

/// 扫屏保目录，找出所有 `bg_ss*.png`。
///
/// 用 `bg_ss` 前缀 + `.png` 后缀来筛，和系统自己的命名一致
/// （`bg_ss00.png`、`bg_ss01.png` …）。
fn scan() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    let Ok(rd) = std::fs::read_dir(SS_DIR) else {
        log::warn!("[屏保] 打不开 {SS_DIR}，挂起时将不显示屏保");
        return out;
    };

    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        // 跳过 bg_ss00.sdr 这种附属目录
        if e.path().is_dir() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if lower.starts_with("bg_ss") && lower.ends_with(".png") {
            out.push(e.path());
        }
    }

    // 排序保证轮换顺序稳定（bg_ss00 → bg_ss01 → …）
    out.sort();
    out
}

/// 取下一张屏保图。
///
/// 每次调用换一张（目录里有多张时），返回解码好的像素缓冲。
/// 一张都取不到就返回 `None`。
pub fn next_image() -> Option<PixelBuffer> {
    let Ok(mut c) = CACHE.lock() else {
        return None; // 锁中毒：宁可没屏保，也别 panic
    };

    if !c.scanned {
        c.scanned = true;
        c.paths = scan();
        c.loaded = vec![None; c.paths.len()];
        if c.paths.is_empty() {
            log::warn!("[屏保] {SS_DIR} 里没有 bg_ss*.png");
        } else {
            log::info!("[屏保] 找到 {} 张：{:?}", c.paths.len(),
                c.paths.iter().filter_map(|p| p.file_name()).collect::<Vec<_>>());
        }
    }

    if c.paths.is_empty() {
        return None;
    }

    // 轮换：每挂起一次换下一张
    let n = c.paths.len();
    for _ in 0..n {
        let i = c.next % n;
        c.next = c.next.wrapping_add(1);

        // 已加载过就直接用
        if let Some(img) = &c.loaded[i] {
            return Some(img.clone());
        }

        // 没加载过：读 + 解码
        let path = c.paths[i].clone();
        match std::fs::read(&path) {
            Ok(bytes) => match render::decode(&bytes) {
                Ok(buf) => {
                    // 交给面板之前先处理一遍：近白推纯白 + 抖动到 16 级。
                    // 不做的话背景会是一片片浅灰（「脏」），细节处出现色带。
                    // 详见 `render::prepare_for_panel` 的说明。
                    let prepared = render::prepare_for_panel(&buf);
                    log::info!(
                        "[屏保] 已加载 {}（{}x{}，已按面板 16 级处理）",
                        path.display(),
                        prepared.width(),
                        prepared.height()
                    );
                    c.loaded[i] = Some(prepared.clone());
                    return Some(prepared);
                }
                Err(e) => {
                    log::warn!("[屏保] 解码 {} 失败：{e}", path.display());
                    // loaded[i] 保持 None，下次换下一张
                }
            },
            Err(e) => {
                log::warn!("[屏保] 读 {} 失败：{e}", path.display());
            }
        }
    }

    log::warn!("[屏保] {} 张图都读不出来，这次不显示屏保", c.paths.len());
    None
}

//! 刷新策略：什么时候该用「全屏刷新」而不是「局部刷新」。
//!
//! # 为什么要这个东西
//!
//! 电子墨水屏的局部刷新（`UPDATE_MODE_PARTIAL`）快、不闪，但**会累积残影**：
//! 每次只重画变化的那块区域，旧像素的电荷残留一点，几十次之后上一页的字
//! 就会透出来。清掉残影只能靠全屏刷新（`UPDATE_MODE_FULL` + GC16 波形），
//! 代价是整屏黑一下再白回来，三百毫秒左右。
//!
//! 上游 0.7.1 的做法（`platform.rs` 的绘制回调）是：
//!
//! ```text
//! if rotation_changed { refresh_full() } else { refresh_region(...) }
//! ```
//!
//! 也就是**除了启动和旋转，永远走局部刷新**。翻一百页就攒一百次残影，
//! 在漫画这种大面积灰度图上尤其明显。
//!
//! 这里补上「每 N 次刷新强制全刷一次」。
//!
//! # 为什么必须是「代替」而不是「额外刷一次」
//!
//! 如果先局部刷再全刷，等于同一帧刷两遍：多一次可见的闪动，还慢一倍。
//! 所以判断要发生在**发 ioctl 之前** —— 这也是为什么这段逻辑必须在后端里，
//! 而不是在应用层。应用层拿不到这一帧的脏矩形，也拦不住后端的刷新调用。
//!
//! # 计数的是「刷新」不是「翻页」
//!
//! 名字上更接近「每 N 页全刷一次」，但实现上数的是**局部刷新次数**。
//! 原因：应用层并不知道哪次重绘对应一次翻页 —— 翻页、开菜单、载入中提示
//! 都是重绘，都会留残影。按重绘次数计数，覆盖得更全，也不需要应用层配合。
//!
//! 副作用是：如果某一帧没有脏矩形（`draw_if_needed` 里压根没渲染），
//! 那一次不算数 —— 这是对的，没刷新就不会产生残影。
//!
//! # ⚠️ 小面积更新不算数（`MIN_AREA_PERCENT`）
//!
//! 这条是后补的，因为踩过一个很显眼的坑：**加载圈一直转 → 整屏不停闪黑白**。
//!
//! 链路是这样串起来的：
//!
//! ```text
//! 加载圈每 250ms 换一格
//!   → 每次都是一次重绘
//!   → 按上面的「数刷新次数」规则，每 N 次（真机 N=3）就全刷
//!   → 450~750ms 一次全屏黑→白
//!   → 用户看到的就是「转圈的时候整个屏幕一直在闪」
//! ```
//!
//! 但那个圈只有 44×44（占面板 0.1%），**根本不会积累可见残影**。
//! 全刷的代价（整屏闪一下、三百毫秒）和它的收益完全不成比例。
//!
//! 所以现在按**脏区域面积**分档：
//!
//! * 小于 `MIN_AREA_PERCENT`（2%）→ 只是一次局部刷新，**不计数、不触发全刷**；
//! * 大于等于它 → 照旧计数，攒够 N 次全刷一次。
//!
//! 2% 这个阈值：加载圈 0.1%、状态条一行约 5%（算大）、整页翻页 100%。
//! 取 2% 能干净地把「小控件变化」和「真的换了内容」分开。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 默认每多少次局部刷新插一次全刷。
///
/// 挑了 1：局部刷新在 Kindle 上带来的收益（快、不闪）远大于一次全刷的代价，
/// 而且残影在漫画上特别显眼。想更省事就调大。
pub const DEFAULT_INTERVAL: u32 = 1;

/// 间隔的上限。再大就等于「关掉全刷」，不如直接用 0 表达。
pub const MAX_INTERVAL: u32 = 999;

/// 脏区域小于「面板面积 × 这个百分比」时，不算一次会积残影的刷新。
///
/// 详细理由见文件头那段。取值依据（PW5 面板 1236×1648 = 203 万像素）：
///
/// | 变化 | 面积占比 |
/// |---|---|
/// | 加载圈 44×44 | 0.1% |
/// | 列表一行 | ~5% |
/// | 整页翻页 | 100% |
///
/// 2% 卡在「小控件」和「真的换了内容」之间。
pub const MIN_AREA_PERCENT: u64 = 2;

/// 局部刷新计数器 + 间隔配置。
///
/// 通过 [`KindleBackend::full_refresh_policy`](crate::KindleBackend::full_refresh_policy)
/// 拿到，是 `Arc`，可以跨线程克隆。
#[derive(Debug)]
pub struct RefreshPolicy {
    /// 已经连续做了多少次局部刷新
    since_full: AtomicU32,
    /// 每多少次局部刷新插一次全刷。**0 = 从不主动全刷**（回到上游行为）。
    interval: AtomicU32,
    /// 下一次刷新无条件全刷一次（用户刚在设置里改完，要立刻看到效果）。
    ///
    /// 一发的，用完自动清掉。没有把它并进 `set_interval` 是因为**启动时也会
    /// 调 set_interval 装载上次存的配置**，那时强制全刷纯属浪费 ——
    /// 后端在进事件循环前已经全刷过一次了。
    force_next: AtomicBool,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_INTERVAL)
    }
}

impl RefreshPolicy {
    pub fn new(interval: u32) -> Self {
        Self {
            since_full: AtomicU32::new(0),
            interval: AtomicU32::new(clamp_interval(interval)),
            force_next: AtomicBool::new(false),
        }
    }

    /// 设置间隔。0 表示关掉主动全刷（回到上游的行为：只有旋转才全刷）。
    ///
    /// 计数清零，也就是「从这里重新数」。**不会**顺手强制全刷一次 ——
    /// 想立刻看到效果，另外调 [`force_next_refresh`](Self::force_next_refresh)。
    /// 分开的理由见 `force_next` 字段的注释：启动装载配置时也会走这里。
    pub fn set_interval(&self, interval: u32) {
        self.interval
            .store(clamp_interval(interval), Ordering::Relaxed);
        self.since_full.store(0, Ordering::Relaxed);
    }

    /// 当前的间隔（已经过上限夹取）。
    pub fn interval(&self) -> u32 {
        self.interval.load(Ordering::Relaxed)
    }

    /// 下一次刷新无条件全刷一次 —— 即便间隔还没到、即便间隔是 0。
    ///
    /// 用途：用户在设置里刚改完间隔，让他马上看到一次全刷，确认设置生效了。
    /// 一发的，用掉就没了。
    pub fn force_next_refresh(&self) {
        self.force_next.store(true, Ordering::Relaxed);
    }

    /// 这一次局部刷新该不该换成全刷？
    ///
    /// `dirty_area` / `panel_area` 是**像素个数**，由后端算出脏矩形的面积传进来。
    /// 面积太小的更新（见 [`MIN_AREA_PERCENT`]）直接当成普通局部刷新：
    /// 不计数、不触发全刷 —— 否则像加载圈这种高频小重绘会把计数刷爆。
    ///
    /// 次数到点返回 true 并把计数清零；否则计数 +1 返回 false。
    pub(crate) fn wants_full_refresh(&self, dirty_area: u64, panel_area: u64) -> bool {
        // 手动要求的那次优先 —— **不受面积判断影响**。
        // 用户在设置里改完间隔想立刻看到效果，哪怕这一帧只有几个像素变了。
        if self.force_next.swap(false, Ordering::Relaxed) {
            self.since_full.store(0, Ordering::Relaxed);
            return true;
        }

        // 小面积更新：不进计数、不触发全刷。
        //
        // `panel_area == 0` 说明后端没传（理论上到不了），那种情况按「算大的」处理，
        // 宁可多刷一次也别把全刷功能整个废掉。
        if panel_area > 0
            && dirty_area.saturating_mul(100)
                < panel_area.saturating_mul(MIN_AREA_PERCENT)
        {
            return false;
        }

        let interval = self.interval.load(Ordering::Relaxed);
        if interval == 0 {
            return false; // 0 = 关掉，永远走局部
        }
        // fetch_add 返回的是加之前的值，所以第一次调用时 old = 0。
        // old + 1 >= interval 意味着「算上这一次，已经攒够 interval 次了」。
        let old = self.since_full.fetch_add(1, Ordering::Relaxed);
        if old.saturating_add(1) >= interval {
            self.since_full.store(0, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// 旋转之类的场合已经全刷过了，计数归零 ——
    /// 不然紧接着又会触发一次多余的全刷。
    pub(crate) fn note_full_refresh(&self) {
        self.since_full.store(0, Ordering::Relaxed);
    }
}

fn clamp_interval(v: u32) -> u32 {
    v.min(MAX_INTERVAL)
}

/// 测试用的面板面积（PW5 实测 1236×1648）。
#[cfg(test)]
const PANEL: u64 = 1236 * 1648;

/// 测试里「一整页」的脏区域 = 整个面板。
#[cfg(test)]
const FULL: u64 = PANEL;

/// 测试里「加载圈」那种小区域：44×44。
#[cfg(test)]
const SPINNER: u64 = 44 * 44;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 间隔为1时每次都全刷() {
        let p = RefreshPolicy::new(1);
        for i in 0..5 {
            assert!(p.wants_full_refresh(FULL, PANEL), "第 {i} 次应该全刷");
        }
    }

    #[test]
    fn 间隔为3时每三次一次全刷() {
        let p = RefreshPolicy::new(3);
        let got: Vec<bool> = (0..7).map(|_| p.wants_full_refresh(FULL, PANEL)).collect();
        assert_eq!(
            got,
            vec![false, false, true, false, false, true, false],
            "节奏不对"
        );
    }

    #[test]
    fn 间隔为0等于关掉() {
        let p = RefreshPolicy::new(0);
        for _ in 0..50 {
            assert!(!p.wants_full_refresh(FULL, PANEL), "关掉之后不该再全刷");
        }
        assert_eq!(p.interval(), 0);
    }

    #[test]
    fn 手动要求的那次一定会全刷() {
        let p = RefreshPolicy::new(10);
        // 先攒几次，让计数处于中间状态
        for _ in 0..4 {
            assert!(!p.wants_full_refresh(FULL, PANEL));
        }
        // 用户改完设置，要求立刻看到一次
        p.force_next_refresh();
        assert!(p.wants_full_refresh(FULL, PANEL), "手动要求的全刷没生效");
        // 一发即止：紧接着又回到正常节奏
        for _ in 0..9 {
            assert!(!p.wants_full_refresh(FULL, PANEL));
        }
        assert!(p.wants_full_refresh(FULL, PANEL), "第 10 次该到了");
    }

    #[test]
    fn 手动要求能穿透关掉的间隔() {
        let p = RefreshPolicy::new(0);
        assert!(!p.wants_full_refresh(FULL, PANEL));
        p.force_next_refresh();
        assert!(p.wants_full_refresh(FULL, PANEL), "间隔为 0 时手动要求也该生效");
        // 用掉之后就恢复成「永远不主动全刷」
        assert!(!p.wants_full_refresh(FULL, PANEL));
    }

    #[test]
    fn 改设置本身不强制全刷() {
        // 启动时装载配置也走 set_interval，那时强制全刷纯属浪费
        let p = RefreshPolicy::new(3);
        p.set_interval(5);
        assert!(!p.wants_full_refresh(FULL, PANEL), "set_interval 不该附带一次全刷");
    }

    #[test]
    fn 全刷之后计数归零() {
        let p = RefreshPolicy::new(2);
        assert!(!p.wants_full_refresh(FULL, PANEL));
        assert!(p.wants_full_refresh(FULL, PANEL));
        // 刚全刷过，接下来两次又该是「否、是」
        assert!(!p.wants_full_refresh(FULL, PANEL));
        assert!(p.wants_full_refresh(FULL, PANEL));
    }

    #[test]
    fn note_full_refresh_会重置计数() {
        let p = RefreshPolicy::new(3);
        assert!(!p.wants_full_refresh(FULL, PANEL));
        assert!(!p.wants_full_refresh(FULL, PANEL));
        p.note_full_refresh(); // 比如旋转已经全刷过了
        assert!(!p.wants_full_refresh(FULL, PANEL), "重置后不该马上又全刷");
        assert!(!p.wants_full_refresh(FULL, PANEL));
        assert!(p.wants_full_refresh(FULL, PANEL));
    }

    #[test]
    fn 间隔被夹在上限内() {
        let p = RefreshPolicy::new(u32::MAX);
        assert_eq!(p.interval(), MAX_INTERVAL);
    }

    // ------------------------------------------------ 小面积更新不触发全刷
    //
    // 这一组是为了修「加载圈一转，整屏不停闪黑白」：
    // 圈每 250ms 换一格 → 每次都是一次重绘 → 按旧规则每 N 次就全刷。
    // 圈只有 44×44（0.1%），不该被算进去。

    /// ⭐ 小面积更新**永远不触发全刷**，哪怕间隔是 1。
    #[test]
    fn 小面积更新不触发全刷() {
        for interval in [1u32, 2, 3, 10] {
            let p = RefreshPolicy::new(interval);
            for i in 0..50 {
                assert!(
                    !p.wants_full_refresh(SPINNER, PANEL),
                    "间隔 {interval} 时第 {i} 次小更新不该全刷"
                );
            }
        }
    }

    /// 小面积更新也**不占用计数** —— 它和整页刷新完全不相干。
    ///
    /// 也就是说：转 100 次圈之后，接着翻第一页仍然按「第 1 次」算。
    #[test]
    fn 小面积更新不占计数() {
        let p = RefreshPolicy::new(3);
        // 先转 100 次圈
        for _ in 0..100 {
            assert!(!p.wants_full_refresh(SPINNER, PANEL));
        }
        // 然后翻三页，节奏应该和「从来没转过圈」一模一样
        assert!(!p.wants_full_refresh(FULL, PANEL), "第 1 页");
        assert!(!p.wants_full_refresh(FULL, PANEL), "第 2 页");
        assert!(p.wants_full_refresh(FULL, PANEL), "第 3 页该全刷了");
    }

    /// 面积刚好在阈值附近的两侧行为要相反。
    ///
    /// 阈值 = 面板 × 2%。用 1.9% 和 2.1% 各测一次。
    #[test]
    fn 面积阈值两侧() {
        let panel = 1_000_000u64;
        let below = panel * 19 / 1000; // 1.9%
        let above = panel * 21 / 1000; // 2.1%

        let p = RefreshPolicy::new(1);
        assert!(!p.wants_full_refresh(below, panel), "1.9% 不该全刷");
        assert!(
            p.wants_full_refresh(above, panel),
            "2.1% 且间隔 1 时应该全刷"
        );
    }

    /// 面板面积为 0（后端没传）时按「算大的」处理，别把全刷功能废掉。
    #[test]
    fn 面板面积为0时退回旧行为() {
        let p = RefreshPolicy::new(1);
        assert!(p.wants_full_refresh(1, 0), "间隔 1 时该全刷");
    }

    /// 手动的 `force_next_refresh` 优先级高于面积判断 ——
    /// 用户改完设置想立刻看到效果，哪怕这一帧只有几个像素变了。
    #[test]
    fn 手动要求能穿透小面积判断() {
        let p = RefreshPolicy::new(3);
        p.force_next_refresh();
        assert!(
            p.wants_full_refresh(SPINNER, PANEL),
            "手动要求的全刷被面积判断拦掉了"
        );
    }
}

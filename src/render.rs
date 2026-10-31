//! 面板常量与图像解码 / 裁剪 / 适配。
//!
//! ⚠️ 这里的像素值和 `ui/app.slint` 里的布局是**同一套约定**，改一处必须改另一处。
//!
//! # 两种屏幕布局
//!
//! **列表类界面**（首页/历史/搜索/目录/设置）保留上下栏：
//!
//! ```text
//! ┌──────────────────────────┐  y=0
//! │ 顶栏  TOP_BAR = 64        │   状态 + 页码
//! ├──────────────────────────┤  y=64
//! │ 内容区 LIST_H = 1488      │
//! ├──────────────────────────┤  y=1552
//! │ 底栏  BOTTOM_BAR = 96     │   返回 / 上一页 / 下一页 / 退出
//! └──────────────────────────┘  y=1648
//! ```
//!
//! **阅读器占满整屏**，没有任何栏 —— 漫画要多给一像素就多给一像素：
//!
//! ```text
//! ┌──────────────────────────┐  y=0
//! │                          │
//! │   整屏画布 1236×1648      │
//! │                          │
//! └──────────────────────────┘  y=1648
//! ```
//!
//! 阅读器为什么这么在意「一屏到底是多少像素」：长条漫要由客户端自己按屏裁剪，
//! 裁多了会出现半截图，裁少了会重复。所以 VIEW_H 必须是真的可视高度。

use image::imageops::FilterType;

/// 面板宽度，来自 PW5 实测规格。
pub const PANEL_W: usize = 1236;
/// 面板高度。
pub const PANEL_H: usize = 1648;
/// 顶栏高度（列表界面用，阅读器不用）。
///
/// 这三个常量**没有 Rust 代码在算**，它们是把 `ui/app.slint` 里的布局写成数字，
/// 放在这里当文档用 —— 改界面时对照着看，免得算错行高。
#[allow(dead_code)]
pub const TOP_BAR: usize = 64;
/// 底栏高度（列表界面用，阅读器不用）。
#[allow(dead_code)]
pub const BOTTOM_BAR: usize = 96;

/// 列表界面的内容区高度。
#[allow(dead_code)]
pub const LIST_H: usize = PANEL_H - TOP_BAR - BOTTOM_BAR;

/// 阅读器画布 = 整屏。
pub const VIEW_W: usize = PANEL_W;
pub const VIEW_H: usize = PANEL_H;

/// 长条漫判定阈值，和服务端 `jm_kindle_raw.py` 的 `LONG_STRIP_RATIO` 保持一致。
/// 超过这个高宽比就按纵向滚动处理，而不是整页塞进一屏。
pub const LONG_STRIP_RATIO: f32 = 2.0;

/// 普通单页解码后的像素上限。
///
/// 屏幕只有 1236×1648 = **2.04 M 像素**，多留的那些只是白占内存。
/// 按 `PLAN-服务端方案.md` 第 4.3 节下调到 2.5 M：每张 RGBA 最多约 10 MB，
/// 峰值约 18 MB，给 1 GB 的机器留足余量。
///
/// ⚠️ 它只限制**留下来**的那份，不限制解码峰值 —— `image` crate 会先把整张
///    图解出来再缩，这一点躲不掉（除非换流式解码器）。
const MAX_PIXELS: u64 = 2_500_000;

/// 长条漫的像素上限，**比普通页高得多**。
///
/// 长条漫按宽度缩放、纵向切片，宽度就是清晰度：如果也按 2.5 M 封顶，
/// 一条 1236×20000 的图会被缩到 386 宽，放到 1236 宽的屏上会糊得没法看。
/// 8 M 像素 ≈ 32 MB RGBA，是这台机器能舒服承受的量级。
const MAX_PIXELS_LONG: u64 = 8_000_000;

/// 列表行里那个小封面框的尺寸上限（表格里的参考值）。
///
/// ⚠️ 现在**运行时不再用它**：列表行（`ListLine`）的框写死 72×96 + contain，
///    搜索页的网格用自己的 340 —— 这两个常量只剩单元测试和文档在引用。
///    保留是因为「方图给方框」那组测试用它们当典型上限；删了测试就得改成魔数。
///
/// 注意它只是**显示框**的上限；真正解码封面时用 [`THUMB_MAX`]（更大）。
#[allow(dead_code)]
pub const THUMB_W: u32 = 168;
#[allow(dead_code)]
pub const THUMB_H: u32 = 232;

/// 封面**解码**时的最大边长。
///
/// 搜索页改成了三列封面网格，每格约 316px —— 原来按 168 解码再放大到 316，
/// 墨水屏上会明显糊掉。统一解码到这个尺寸，列表行靠 Slint 缩小显示
/// （缩小不会糊），两种界面共用同一份缓存，不会出现「先开历史页再搜同一条
/// 就拿到小图」的尺寸不一致问题。
pub const THUMB_MAX: u32 = 360;
/// 详情页顶部那张大一点的封面。
pub const COVER_W: u32 = 240;
pub const COVER_H: u32 = 332;

pub type PixelBuffer = slint::SharedPixelBuffer<slint::Rgba8Pixel>;

// ---------------------------------------------------------------- 适配模式

/// 图片怎么铺满屏幕。
///
/// 两种都**不改变宽高比**（不拉伸），区别只是让哪条边贴满：
///
/// * `FitHeight` 竖边适应：高度填满，宽度可能留白。漫画单页大多是「比屏幕更高更窄」，
///   所以这个模式能把整页一次看完 —— **默认就是它**。
/// * `FitWidth` 横边适应：宽度填满，高度可能超出（超出部分裁掉）。
///   跨页大图（一张图两页宽）用它刚好填满，不会被压小。
///
/// 长条漫不走这两个模式 —— 它按宽度缩放后纵向滚动，见 `slice_to_canvas`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FitMode {
    FitHeight,
    FitWidth,
}

impl FitMode {
    /// 菜单按钮上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            FitMode::FitHeight => "竖边适应",
            FitMode::FitWidth => "横边适应",
        }
    }

    /// 点一下就切到另一个。
    pub fn toggled(self) -> Self {
        match self {
            FitMode::FitHeight => FitMode::FitWidth,
            FitMode::FitWidth => FitMode::FitHeight,
        }
    }
}

impl Default for FitMode {
    fn default() -> Self {
        // 竖边适应当默认：漫画单页通常比屏幕更「高瘦」，
        // 按高度缩放正好整页落进屏幕，不用裁也不用滚。
        FitMode::FitHeight
    }
}

// ---------------------------------------------------------------- 解码

/// 解码结果：像素 + **缩放前**的原始尺寸。
///
/// 带上原始高度是为了禁漫的「横条重排」解密：分段边界是按原图高度算的
/// （`h % num`），如果解码时已经降过采样，必须用原高度做比例映射，
/// 否则拼接处会出现 1~2 行的错位缝。
pub struct Decoded {
    pub buf: PixelBuffer,
    /// 缩放前的原始宽度。目前只有测试用得到（解密只用高度），
    /// 留着是为了「原始尺寸」这个概念完整，别再被拉去当别的用途。
    #[allow(dead_code)]
    pub orig_w: u32,
    /// 缩放前的原始高度。禁漫解密靠它做分段边界的比例映射。
    pub orig_h: u32,
}

/// 解码一张图（并带出原始尺寸）。
///
/// 中间那步降采样是给长条漫准备的：`image` crate 会把整张图先解出来，
/// 所以峰值内存仍然等于原图大小（这点躲不掉，除非换成流式解码器），
/// 但**留下来的**那份被压到预算内，翻页时不会因为同时驻留几张巨图而爆内存。
///
/// 预算分两档：普通页 2.5 M，长条漫 8 M（理由见两个常量的注释）。
pub fn decode_meta(bytes: &[u8]) -> Result<Decoded, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("解码失败: {e}"))?;

    let (w, h) = (img.width(), img.height());
    let (orig_w, orig_h) = (w, h);
    let pixels = w as u64 * h as u64;
    let is_long = w > 0 && h as f32 / w as f32 > LONG_STRIP_RATIO;
    let budget = if is_long { MAX_PIXELS_LONG } else { MAX_PIXELS };

    let img = if pixels > budget {
        let k = (budget as f64 / pixels as f64).sqrt();
        let nw = ((w as f64 * k).round() as u32).max(1);
        let nh = ((h as f64 * k).round() as u32).max(1);
        img.resize(nw, nh, FilterType::Triangle)
    } else {
        img
    };

    let rgba = img.to_rgba8();
    Ok(Decoded {
        buf: PixelBuffer::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()),
        orig_w,
        orig_h,
    })
}

/// 只要像素、不要原始尺寸时的简写。
///
/// ⚠️ 主流程现在都走 [`decode_meta`]（解密需要原始高度）。保留这个包装
///    是为了屏保/测试这类「不在乎原图尺寸」的调用，别删。
#[allow(dead_code)]
pub fn decode(bytes: &[u8]) -> Result<PixelBuffer, String> {
    decode_meta(bytes).map(|d| d.buf)
}

/// 解码成缩略图（保持比例，只缩不放）。
/// 列表里的封面用它，避免为了一个小格子把整张封面留在内存里。
pub fn decode_thumb(bytes: &[u8], max_w: u32, max_h: u32) -> Result<PixelBuffer, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("解码失败: {e}"))?;
    let thumb = img.thumbnail(max_w, max_h); // thumbnail 只缩小不放大
    let rgba = thumb.to_rgba8();
    Ok(PixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

// ---------------------------------------------------------------- 面板适配

// 下面这几项只给**屏保**用，而屏保只在真机/模拟器的 ARM 构建里编进来
// （见 src/screensaver.rs）。桌面预览用不到，所以那边会报 dead_code。
#[cfg_attr(
    not(all(feature = "kindle", target_arch = "arm")),
    allow(dead_code)
)]

/// 面板的灰阶档数。PW5 用 GC16 波形刷全屏，而 GC16 的「16」就是 16 级灰阶。
///
/// 佐证：这台机器上 `linkss` 屏保插件预处理出来的图，正好被量化成
/// `0,17,34,…,238,255` 这 16 档，且 248~254 区间**一个像素都没有** ——
/// 做那套东西的人显然知道这块屏只有 16 档。
pub const PANEL_LEVELS: u32 = 16;

#[cfg_attr(
    not(all(feature = "kindle", target_arch = "arm")),
    allow(dead_code)
)]
/// 近白阈值 = 面板上 238 档和 255 档的中点（246.5）向上取整。
///
/// 比它亮的像素一律推成纯白。**这是修「屏保背景脏」的关键**：
/// 用户的屏保图有 37% 的像素落在 248~254，在 8 位下和纯白毫无区别，
/// 但面板 238 和 255 之间没有档位，这一段必然被驱动推到其中一档 ——
/// 推到 238 就是成片浅灰，也就是肉眼看到的「脏」和「色块」。
const NEAR_WHITE: u8 = 247;

#[cfg_attr(
    not(all(feature = "kindle", target_arch = "arm")),
    allow(dead_code)
)]
/// 把图处理成「电子墨水面板能干净显示」的样子。屏保用它。
///
/// 两步：
///
/// 1. **近白吸附**（≥247 → 纯白）。让背景确定性地变成纯白，
///    不再取决于驱动怎么把 8 位映射到 16 档。
///
/// 2. **Floyd–Steinberg 抖动**到 16 级网格。其余部分（人物、渐变）
///    量化时会产生边界分明的色带，抖动把它摊成网点，看上去是连续的灰度。
///    KOReader 的屏保也开抖动（`screensaverwidget.lua` 里 `self.dithered = true`），
///    这是同一件事。
///
/// 注意：**只给屏保用，漫画不走这条路**。漫画本来就是给 16 档屏看的灰度图，
/// 再抖一遍只会平白加上颗粒感。
pub fn prepare_for_panel(buf: &PixelBuffer) -> PixelBuffer {
    let w = buf.width() as usize;
    let h = buf.height() as usize;

    let mut out = PixelBuffer::new(w as u32, h as u32);
    if w == 0 || h == 0 {
        return out;
    }

    // 先算亮度。公式和 Kindle 后端里那一步**必须一致**，
    // 否则我们算出来的灰度和实际写进帧缓冲的会对不上。
    let src = buf.as_bytes();
    let mut work: Vec<f32> = Vec::with_capacity(w * h);
    let mut snapped: Vec<bool> = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let p = &src[i * 4..i * 4 + 4];
        let luma = ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32) >> 8) as u8;
        let (v, s) = if luma >= NEAR_WHITE {
            (255.0f32, true)
        } else {
            (luma as f32, false)
        };
        work.push(v);
        snapped.push(s);
    }

    let step = 255.0 / (PANEL_LEVELS - 1) as f32; // 17.0

    // 误差扩散要往**右边和下边**加，所以按行从左到右扫。
    // 已经吸附成纯白的像素不再接收误差，否则会被邻居的错误拉回灰档，
    // 吸附就白做了。
    let diffuse = |work: &mut Vec<f32>, i: usize, err: f32, factor: f32| {
        if !snapped[i] {
            work[i] += err * factor;
        }
    };

    {
        let dst = out.make_mut_bytes();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let old = work[i];
                let q = ((old / step).round() * step).clamp(0.0, 255.0);
                let q = q as u8;
                let err = old - q as f32;

                if !snapped[i] {
                    // Floyd–Steinberg 的四个权重 7/3/5/1，除以 16
                    if x + 1 < w {
                        diffuse(&mut work, i + 1, err, 7.0 / 16.0);
                    }
                    if y + 1 < h {
                        if x > 0 {
                            diffuse(&mut work, i + w - 1, err, 3.0 / 16.0);
                        }
                        diffuse(&mut work, i + w, err, 5.0 / 16.0);
                        if x + 1 < w {
                            diffuse(&mut work, i + w + 1, err, 1.0 / 16.0);
                        }
                    }
                }

                let o = i * 4;
                dst[o] = q;
                dst[o + 1] = q;
                dst[o + 2] = q;
                dst[o + 3] = 255;
            }
        }
    }

    out
}

/// 一张图等比放进 `max_w × max_h` 之后，实际占多大。
///
/// 给封面用：**框的尺寸跟着图走** —— 方图给方框、竖图给竖框，
/// 这样图片框和图片永远严丝合缝，不会出现「框是长的、图是方的、
/// 上下一条空白」那种情况。
///
/// 注意这里**会放大**：源图可能只有 100×100，但显示尺寸该是 168×168。
/// 返回的是「显示尺寸」，不是「源图尺寸」。
///
/// 尺寸为 0（还没取到封面）时返回最大框，当占位用。
pub fn fit_box(sw: u32, sh: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || max_w == 0 || max_h == 0 {
        return (max_w, max_h);
    }
    let scale = f64::min(max_w as f64 / sw as f64, max_h as f64 / sh as f64);
    let w = ((sw as f64 * scale).round() as u32).clamp(1, max_w);
    let h = ((sh as f64 * scale).round() as u32).clamp(1, max_h);
    (w, h)
}

#[cfg_attr(
    not(all(feature = "kindle", target_arch = "arm")),
    allow(dead_code)
)]
/// 一整块纯白。屏保挂起前先铺它、全刷一次清残影用。
pub fn white_image(w: u32, h: u32) -> PixelBuffer {
    let mut b = PixelBuffer::new(w, h);
    b.make_mut_bytes().fill(255);
    b
}

/// 是不是长条漫（需要纵向滚动的那种）。
pub fn is_long_strip(buf: &PixelBuffer) -> bool {
    if buf.width() == 0 {
        return false;
    }
    buf.height() as f32 / buf.width() as f32 > LONG_STRIP_RATIO
}

// ---------------------------------------------------------------- 普通页：整图适配

// ---------------------------------------------------------------- 重采样
//
// ⚠️ 这里原来是**最近邻**，用户反馈「内置服务端比电脑服务端糊」之后才换掉的。
//
// 差别在链路里：
//   * 电脑端 `jm_kindle_raw.py` 先用 **LANCZOS** 把图缩到屏幕尺寸再发；
//   * 内置服务端发原图，客户端的 `fit_to_canvas` 要自己缩 1280×1818 → 1160×1648。
//
// 最近邻在这种情况下是「隔 10 个像素抽 1 个」，**直接把别人的像素扔掉**：
// 细线会断、文字边缘会起毛刺。实测同一张真实页面（缩放 0.906）：
//
// | 缩放方式 | 与 LANCZOS 的平均绝对差 | 尖锐度 |
// |---|---|---|
// | 最近邻 | **2.40** | 375.7 |
// | 三角滤波 | 1.26 | 233.7 |
// | LANCZOS（电脑端） | —（基准） | 348.4 |
//
// 注意最近邻的「尖锐度」反而最高 —— 那不是清晰，是**锯齿伪高频**，
// 在墨水屏上看着就是边缘发脏。
//
// 现在的做法：
//   * **缩小**（目标比源小）→ 面积平均（box filter）。每个目标像素 =
//     它覆盖的那段源像素的加权平均，信息不丢、也不产生伪高频。这是
//     降采样在数学上正确的做法。
//   * **放大**（目标比源大）→ 双线性。最近邻放大会出方块，墨水屏上很明显。
//
// 定点整数实现：每个轴的权重是 `1<<10`（1024）分之一的定点数。
//
// 为什么是 10 位而不是 16 位：重采样是**两维**的，一个输出像素的权重是
// 横竖两个权重之积。10+10 = 20 位，乘以最大像素值 255 是 2.67e8，u32
// 装得下；16+16 就是 2^32，必须换成 u64（ARM 上 u64 乘法慢一截）。
// 1/1024 的量化对 8 位输出完全够用。
const TAP_SHIFT: u32 = 10;
/// 每个轴的定点分母。
const FIXED_ONE: u32 = 1 << TAP_SHIFT;

/// 一维重采样系数表。
///
/// 把长度 `src_len` 重采样到 `dst_len`：第 `i` 个目标位置的源下标是
/// `indices[span.0..span.1]`，对应权重是 `weights[同一区间]`。
struct Taps {
    starts: Vec<u32>,
    indices: Vec<u32>,
    weights: Vec<u32>,
}

impl Taps {
    fn span(&self, i: usize) -> (usize, usize) {
        (self.starts[i] as usize, self.starts[i + 1] as usize)
    }

    /// 面积平均系数（用于**缩小**）。
    ///
    /// 目标位置 i 覆盖源区间 `[i*ratio, (i+1)*ratio)`；与每个源像素的重叠
    /// 长度就是它的权重。缩小时这些权重之和 = ratio，归一化后正好盖满。
    fn area(src_len: usize, dst_len: usize) -> Self {
        let src_len = src_len.max(1);
        let dst_len = dst_len.max(1);
        let ratio = src_len as f64 / dst_len as f64;

        let mut starts = Vec::with_capacity(dst_len + 1);
        let mut indices = Vec::new();
        let mut weights = Vec::new();

        for i in 0..dst_len {
            starts.push(weights.len() as u32);
            let a = i as f64 * ratio;
            let b = (i + 1) as f64 * ratio;
            let first = (a.floor() as usize).min(src_len - 1);
            let last = ((b.ceil() as usize).max(first + 1)).min(src_len);

            // 先收集「下标 + 浮点权重」，归一化之后再转定点。
            let mut taps: Vec<(usize, f64)> = Vec::new();
            for j in first..last {
                let lo = (j as f64).max(a);
                let hi = ((j + 1) as f64).min(b);
                if hi > lo {
                    taps.push((j, hi - lo));
                }
            }
            let sum: f64 = taps.iter().map(|(_, w)| *w).sum();
            if sum <= 0.0 || taps.is_empty() {
                // 理论上到不了（ratio >= 1 时每格至少覆盖一个像素）。
                // 兜底成最近邻，绝不 panic。
                indices.push(first as u32);
                weights.push(FIXED_ONE);
                continue;
            }

            // 最后一个用「总和减前面」补足，避免四舍五入让权重和差几。
            let mut acc = 0u32;
            let n = taps.len();
            for (k, (j, w)) in taps.iter().enumerate() {
                let v = if k + 1 == n {
                    FIXED_ONE.saturating_sub(acc)
                } else {
                    ((w / sum) * FIXED_ONE as f64).round() as u32
                };
                acc = acc.saturating_add(v);
                indices.push(*j as u32);
                weights.push(v);
            }
        }
        starts.push(weights.len() as u32);
        Taps { starts, indices, weights }
    }

    /// 双线性系数（用于**放大**）。
    ///
    /// 目标像素中心的源坐标 `(i+0.5)*ratio - 0.5`，取它左右两个源像素按距离加权。
    fn linear(src_len: usize, dst_len: usize) -> Self {
        let src_len = src_len.max(1);
        let dst_len = dst_len.max(1);
        let ratio = src_len as f64 / dst_len as f64;

        let mut starts = Vec::with_capacity(dst_len + 1);
        let mut indices = Vec::new();
        let mut weights = Vec::new();

        for i in 0..dst_len {
            starts.push(weights.len() as u32);
            let center = (i as f64 + 0.5) * ratio - 0.5;
            let floored = center.floor();
            let frac = center - floored;
            let i0 = if floored < 0.0 {
                0
            } else {
                (floored as usize).min(src_len - 1)
            };
            let i1 = (i0 + 1).min(src_len - 1);

            if i0 == i1 || frac <= 0.0 {
                indices.push(i0 as u32);
                weights.push(FIXED_ONE);
            } else {
                let w0 = ((1.0 - frac) * FIXED_ONE as f64).round() as u32;
                let w0 = w0.min(FIXED_ONE);
                indices.push(i0 as u32);
                weights.push(w0);
                if FIXED_ONE > w0 {
                    indices.push(i1 as u32);
                    weights.push(FIXED_ONE - w0);
                }
            }
        }
        starts.push(weights.len() as u32);
        Taps { starts, indices, weights }
    }

    /// 按缩放方向自动选：缩小用面积平均，放大用双线性。
    fn for_scale(src_len: usize, dst_len: usize) -> Self {
        if dst_len < src_len {
            Self::area(src_len, dst_len)
        } else {
            Self::linear(src_len, dst_len)
        }
    }
}

#[inline]
fn to_u8(acc: u32) -> u8 {
    // 四舍五入后夹到 0..=255。acc 最大 = 255 × (1<<20)，u32 装得下。
    const SHIFT: u32 = TAP_SHIFT * 2;
    ((acc + (1 << (SHIFT - 1))) >> SHIFT).min(255) as u8
}

/// 整图重采样：`(sw × sh) → (dw × dh)`。
///
/// **一次遍历算完两维**，不落中间缓冲。
///
/// 早先写成「先横后竖」两趟：中间那张 `dw × sh` 的图对 1280×1818 来说是
/// **8.4 MB**，写一遍再读一遍，光内存带宽就吃掉一半时间。合起来之后
/// 每处理一个输出像素直接对 `横 taps × 竖 taps` 个源像素求和
/// （缩放 0.9 时通常就是 2~4 个），比两趟的 2.2 次取样还少。
fn resample(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let tx = Taps::for_scale(sw, dw);
    let ty = Taps::for_scale(sh, dh);
    let mut out = vec![0u8; dw * dh * 4];
    let src_row_bytes = sw * 4;

    for dy in 0..dh {
        let (ys, ye) = ty.span(dy);
        let drow = dy * dw * 4;
        for dx in 0..dw {
            let (xs, xe) = tx.span(dx);
            // 只累加 RGB。alpha 直接取第一个参与像素 —— 漫画页全是不透明的，
            // 逐通道平均它是白白多花 25% 的乘法（这是最热的内层循环）。
            let mut acc = [0u32; 3];
            // 用切片迭代器而不是按下标取：省掉每次访问的边界检查，
            // 内层循环一共要跑几百万次，差这一点就是几十毫秒。
            let xidx = &tx.indices[xs..xe];
            let xwgt = &tx.weights[xs..xe];
            for ky in ys..ye {
                let wy = ty.weights[ky];
                let srow = ty.indices[ky] as usize * src_row_bytes;
                for (&ix, &wx) in xidx.iter().zip(xwgt) {
                    let so = srow + ix as usize * 4;
                    let w = wy * wx; // ≤ (1<<20)，不会溢出
                    acc[0] += src[so] as u32 * w;
                    acc[1] += src[so + 1] as u32 * w;
                    acc[2] += src[so + 2] as u32 * w;
                }
            }
            let doff = drow + dx * 4;
            out[doff] = to_u8(acc[0]);
            out[doff + 1] = to_u8(acc[1]);
            out[doff + 2] = to_u8(acc[2]);
            // alpha 取横向第一个 tap 所在行（不参与加权）
            let alpha_src = ty.indices[ys] as usize * src_row_bytes
                + tx.indices[xs] as usize * 4
                + 3;
            out[doff + 3] = src[alpha_src];
        }
    }
    out
}

/// 把整张图按 `mode` 缩放后贴到整屏画布上。
///
/// 为什么要自己贴画布，而不是让 Slint 的 `image-fit` 去缩：
///
/// * **可控**。Slint 的 fit 行为（contain/cover/fill）在「图比画布小」时会放大，
///   而我们这里要的是「按哪条边贴满」，得自己算。
/// * **一次算完**。缩放后的画布直接交给 Slint 当普通图片显示，
///   翻页时 Slint 不用再做任何缩放 —— 电子墨水屏上少一层计算就是少一点闪烁。
///
/// 不改变宽高比：缩放比由 `mode` 定，横向纵向**用同一个比例**，
/// 所以图绝不会变形。超出的部分被裁掉（画布外，不画）。
pub fn fit_to_canvas(buf: &PixelBuffer, mode: FitMode) -> PixelBuffer {
    let w = buf.width() as usize;
    let h = buf.height() as usize;

    if w == 0 || h == 0 {
        return PixelBuffer::new(VIEW_W as u32, VIEW_H as u32);
    }

    // 缩放比：横边适应按宽度算，竖边适应按高度算。
    // 这里**允许放大** —— 小图在整屏上留一圈白边很难看，撑满更像「全屏显示」。
    let scale = match mode {
        FitMode::FitWidth => VIEW_W as f32 / w as f32,
        FitMode::FitHeight => VIEW_H as f32 / h as f32,
    };

    let dst_w = ((w as f32) * scale).round().max(1.0) as usize;
    let dst_h = ((h as f32) * scale).round().max(1.0) as usize;

    // 缩放（尺寸相同就跳过，省一次拷贝）
    let scaled = if dst_w == w && dst_h == h {
        None
    } else {
        Some(resample(buf.as_bytes(), w, h, dst_w, dst_h))
    };
    let src: &[u8] = scaled.as_deref().unwrap_or_else(|| buf.as_bytes());

    // 居中摆放；装不下时偏移量为负，超出画布的部分自然被裁掉。
    // 用 isize 是因为偏移可能为负。
    let x_off = (VIEW_W as isize - dst_w as isize) / 2;
    let y_off = (VIEW_H as isize - dst_h as isize) / 2;

    let mut canvas = PixelBuffer::new(VIEW_W as u32, VIEW_H as u32);
    {
        let dst = canvas.make_mut_bytes();
        dst.fill(0xff); // RGBA 全 0xff = 不透明白，正好当电子墨水的底色

        for dy in 0..dst_h {
            let cy = y_off + dy as isize;
            if cy < 0 || cy >= VIEW_H as isize {
                continue; // 这一行在画布外，整行跳过
            }
            let src_row = dy * dst_w * 4;
            let dst_row = (cy as usize) * VIEW_W * 4;

            // 这一行在画布上的有效区间（两边可能被裁）
            let x0 = x_off.max(0) as usize;
            let x1 = ((x_off + dst_w as isize).min(VIEW_W as isize)).max(0) as usize;
            if x1 <= x0 {
                continue;
            }
            let sx0 = x0 as isize - x_off;
            let n = x1 - x0;
            let s = src_row + sx0 as usize * 4;
            let d = dst_row + x0 * 4;
            dst[d..d + n * 4].copy_from_slice(&src[s..s + n * 4]);
        }
    }

    canvas
}

// ---------------------------------------------------------------- 长条漫：按屏切片

/// 长条漫「一屏」对应多少图像像素。
///
/// 长条漫只按宽度适配，所以缩放比是 `VIEW_W / 图宽`。
/// 屏幕上要铺满 VIEW_H 像素高，反过来就需要 `VIEW_H / 缩放比` 个图像像素。
pub fn strip_page_step(buf: &PixelBuffer) -> usize {
    if buf.width() == 0 {
        return VIEW_H;
    }
    let scale = VIEW_W as f32 / buf.width() as f32;
    ((VIEW_H as f32) / scale).round().max(1.0) as usize
}

/// 从长条漫里裁出「一屏」，贴到整屏画布上。
///
/// 返回 `(画布, 实际起始 y, 一屏步长)`。
///
/// 和 `fit_to_canvas` 的区别：这里只做**宽度**适配，高度方向是「取一段」而不是
/// 「缩进去」。因为长条漫可能有几万像素高，整体缩放会把细节压没。
///
/// 起始 y 会被夹到合法范围，所以调用方不用担心越界；返回的实际值可以用来
/// 判断「是不是已经在顶上 / 底上了」。
pub fn slice_to_canvas(strip: &PixelBuffer, y0: usize) -> (PixelBuffer, usize, usize) {
    let w = strip.width() as usize;
    let h = strip.height() as usize;
    let step = strip_page_step(strip);

    if w == 0 || h == 0 {
        return (PixelBuffer::new(VIEW_W as u32, VIEW_H as u32), 0, step);
    }

    // 夹住起始位置：最后一屏从「底边对齐」处开始，不留空白
    let y = y0.min(h.saturating_sub(step));
    let take = step.min(h - y);

    let scale = VIEW_W as f32 / w as f32;

    // ⚠️ **必须夹到画布尺寸**，否则会 panic。
    //
    // 下面那段拷贝是直接写 `dst[di..di+4]`，**没有越界保护**（和 `fit_to_canvas`
    // 不同，那边是靠 `continue` 跳过画布外的行列）。所以 dst_h 只要比 VIEW_H
    // 多 1，写最后一行时就越界。
    //
    // 而它**确实会多**：
    //     step  = round(VIEW_H / scale)       ← 四舍五入，可能向上
    //     dst_h = round(take * scale)         ← 于是 step*scale 比 VIEW_H 略大
    //
    // 实测（真机崩过两次，`range end index 8147716 out of range for slice
    // of length 8147712`，8147712 正是 1236×1648×4）：
    //     图宽 800 → scale = 1.5450
    //     step  = round(1648 / 1.5450) = 1067
    //     dst_h = round(1067 × 1.5450) = round(1648.515) = 1649   ← 多 1 行
    //     写到 dy=1648 时 di+4 = 8147716，正好是日志里那个数。
    // 200..1600 之间**有 42 个图宽档会崩**，不是个别现象。
    //
    // 夹住之后最坏情况是丢掉最后 1~2 行像素（肉眼不可见），
    // 但保证 `y_off + dst_h <= VIEW_H`、`x_off + dst_w <= VIEW_W`，即索引恒合法。
    let dst_w = (((w as f32) * scale).round() as usize).clamp(1, VIEW_W);
    let dst_h = (((take as f32) * scale).round() as usize).clamp(1, VIEW_H);
    let x_off = VIEW_W.saturating_sub(dst_w) / 2;
    let y_off = VIEW_H.saturating_sub(dst_h) / 2;

    // 和 `fit_to_canvas` 一样从最近邻换成正规重采样 —— 长条漫按宽度适配，
    // 图宽 800 时是放大（最近邻出方块），图宽 1600 时是缩小（最近邻丢线）。
    // 两段都走同一套：先纵向把 [y, y+take) 压成 dst_h 行，再横向缩到 dst_w。
    let src = strip.as_bytes();
    let band = &src[y * w * 4..(y + take) * w * 4];
    let scaled = resample(band, w, take, dst_w, dst_h);

    let mut canvas = PixelBuffer::new(VIEW_W as u32, VIEW_H as u32);
    {
        let dst = canvas.make_mut_bytes();
        dst.fill(0xff);

        let n = dst_w * 4;
        for dy in 0..dst_h {
            let s = dy * n;
            let d = ((y_off + dy) * VIEW_W + x_off) * 4;
            dst[d..d + n].copy_from_slice(&scaled[s..s + n]);
        }
    }

    (canvas, y, step)
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> PixelBuffer {
        let mut b = PixelBuffer::new(w, h);
        b.make_mut_bytes().fill(0x80);
        b
    }

    /// 造一张指定灰度的图（RGBA，每像素一个字节灰度值）。
    fn gray_of(w: u32, h: u32, v: u8) -> PixelBuffer {
        let mut b = PixelBuffer::new(w, h);
        for px in b.make_mut_bytes().chunks_exact_mut(4) {
            px[0] = v;
            px[1] = v;
            px[2] = v;
            px[3] = 255;
        }
        b
    }

    /// 面板只有 16 档，`prepare_for_panel` 的输出必须全落在这 16 档上，
    /// 否则就是在让驱动替我们做量化 —— 那正是「背景脏、有色块」的来源。
    #[test]
    fn 面板处理只产生16档() {
        let step = 255 / (PANEL_LEVELS - 1);
        // 一张带各种灰度的图（含最容易出问题的近白段）
        let mut b = PixelBuffer::new(256, 64);
        for (i, px) in b.make_mut_bytes().chunks_exact_mut(4).enumerate() {
            let v = (i % 256) as u8;
            px[0] = v;
            px[1] = v;
            px[2] = v;
            px[3] = 255;
        }
        let out = prepare_for_panel(&b);
        for px in out.as_bytes().chunks_exact(4) {
            let v = px[0];
            assert_eq!(v as u32 % step, 0, "输出 {v} 不在 16 档网格上");
        }
    }

    /// 这是修「屏保背景脏」的核心断言：
    /// 近白（≥247）必须一律变成**纯白**，不能被推成 238 那一档。
    #[test]
    fn 近白被推成纯白() {
        for v in NEAR_WHITE..=255 {
            let img = gray_of(16, 16, v);
            let out = prepare_for_panel(&img);
            for px in out.as_bytes().chunks_exact(4) {
                assert_eq!(
                    px[0], 255,
                    "灰度 {v} 应该被推成纯白，实际 {}",
                    px[0]
                );
            }
        }
    }

    /// 反过来：明显不是白的地方不能被误推成白，否则会吃掉画面细节。
    #[test]
    fn 中灰不会被推成白() {
        for v in [0u8, 64, 128, 200, 230, 240] {
            let img = gray_of(16, 16, v);
            let out = prepare_for_panel(&img);
            let first = out.as_bytes()[0];
            assert!(
                first < 255,
                "灰度 {v} 被推成纯白了，画面会缺一块"
            );
        }
    }

    /// 大面积纯白区域处理完必须**还是**纯白 —— 背景要干净。
    #[test]
    fn 纯白区域保持纯白() {
        let img = gray_of(200, 200, 255);
        let out = prepare_for_panel(&img);
        let n = out.as_bytes().chunks_exact(4).filter(|p| p[0] != 255).count();
        assert_eq!(n, 0, "纯白区域处理后出现了 {n} 个非白像素");
    }

    /// 抖动的意义：一块中间灰度不能变成一整块死板的灰，
    /// 必须有明暗交替（网点），否则就是「色块」。
    #[test]
    fn 抖动产生网点而不是死板色块() {
        // 150 落在 136 和 153 中间，必须靠抖动表达
        let img = gray_of(64, 64, 150);
        let out = prepare_for_panel(&img);
        let vals: Vec<u8> = out.as_bytes().chunks_exact(4).map(|p| p[0]).collect();
        let uniq: std::collections::HashSet<u8> = vals.iter().copied().collect();
        assert!(
            uniq.len() >= 2,
            "整块中间灰被量化成一个值 {uniq:?}，那就是色块而不是网点"
        );
        // 平均值应该贴近原灰度（抖动不改变整体明暗）
        let avg = vals.iter().map(|v| *v as f64).sum::<f64>() / vals.len() as f64;
        assert!(
            (avg - 150.0).abs() < 6.0,
            "抖动后平均灰度 {avg:.1} 偏离原始 150 太多"
        );
    }

    /// **这个 bug 的回归测试**：源图是正方形时，框也必须是正方形。
    ///
    /// 之前的做法是「框写死 168×232（竖的）+ `contain`」，于是正方形封面
    /// 只填了 168×168，上下各空 32px —— 看起来就是「框是长的、图是方的」。
    /// 服务端实际发的封面 45/48 都是正方形，所以这个现象到处都是。
    #[test]
    fn 方图给方框() {
        let (w, h) = fit_box(300, 300, THUMB_W, THUMB_H);
        assert_eq!(w, h, "正方形源图应该得到正方形框，实际 {w}x{h}");
        assert_eq!(w, THUMB_W, "应该撑满宽度");
    }

    /// 竖图给竖框：高度撑满，宽度按比例。
    #[test]
    fn 竖图给竖框() {
        let (w, h) = fit_box(300, 417, THUMB_W, THUMB_H);
        assert_eq!(h, THUMB_H, "竖图应该撑满高度");
        assert!((w as f64 / h as f64 - 300.0 / 417.0).abs() < 0.02, "比例应保持");
    }

    /// 横图给横框：宽度撑满，高度按比例。
    #[test]
    fn 横图给横框() {
        let (w, h) = fit_box(400, 300, THUMB_W, THUMB_H);
        assert_eq!(w, THUMB_W, "横图应该撑满宽度");
        assert!(h < THUMB_H, "高度应该小于框高，实际 {h}");
    }

    /// 框绝不会超出给定的最大范围。
    #[test]
    fn 框不超过上限() {
        for (sw, sh) in [(1, 4000), (4000, 1), (300, 300), (1200, 800), (7, 7)] {
            let (w, h) = fit_box(sw, sh, THUMB_W, THUMB_H);
            assert!(w <= THUMB_W && h <= THUMB_H, "{sw}x{sh} → {w}x{h} 超框了");
            assert!(w >= 1 && h >= 1, "{sw}x{sh} → {w}x{h} 出现 0");
        }
    }

    /// 小图要**放大**到框的尺寸 —— 返回的是显示尺寸，不是源尺寸。
    #[test]
    fn 小图放大到框() {
        let (w, h) = fit_box(50, 50, THUMB_W, THUMB_H);
        assert_eq!((w, h), (THUMB_W, THUMB_W), "50x50 该放大成 168x168");
    }

    /// 尺寸为 0（封面还没取到）时退回最大框，当占位用。
    #[test]
    fn 空图退回最大框() {
        assert_eq!(fit_box(0, 0, THUMB_W, THUMB_H), (THUMB_W, THUMB_H));
        assert_eq!(fit_box(300, 300, 0, 0), (0, 0));
    }

    /// 详情页那个更大的框也要同样成立。
    #[test]
    fn 详情页框跟随图() {
        let (w, h) = fit_box(300, 300, COVER_W, COVER_H);
        assert_eq!((w, h), (COVER_W, COVER_W), "详情页方图也该给方框");
    }

    /// 空图不该 panic（屏保目录里可能有坏文件）。
    #[test]
    fn 面板处理空图安全() {
        let empty = PixelBuffer::new(0, 0);
        let out = prepare_for_panel(&empty);
        assert_eq!(out.width(), 0);
    }

    /// `white_image` 得真的是纯白 —— 挂起前铺它清残影用。
    #[test]
    fn 白图是全白的() {
        let w = white_image(32, 32);
        assert_eq!(w.width(), 32);
        assert!(
            w.as_bytes().iter().all(|b| *b == 255),
            "white_image 里混进了非白字节"
        );
    }

    /// 两种适配模式都必须**保持宽高比**。
    /// 比例一旦算错，图就会被拉伸变形 —— 这是最不能接受的显示问题。
    #[test]
    fn 适配不改变宽高比() {
        // 一张比屏幕更高的图（漫画单页的典型形状）
        let img = solid(1000, 2000);
        let aspect = 1000.0 / 2000.0;

        for mode in [FitMode::FitHeight, FitMode::FitWidth] {
            let canvas = fit_to_canvas(&img, mode);
            assert_eq!(canvas.width(), VIEW_W as u32, "{mode:?} 画布宽度");
            assert_eq!(canvas.height(), VIEW_H as u32, "{mode:?} 画布高度");
        }

        // 竖边适应：高度贴满 → 缩放 = 1648/2000 = 0.824，宽 = 824
        let scale_h = VIEW_H as f32 / 2000.0;
        assert!((1000.0 * scale_h - 824.0).abs() < 1.5, "竖边适应宽度推算");
        // 横向纵向用同一个 scale，所以比例不变
        assert!((1000.0 * scale_h / (2000.0 * scale_h) - aspect).abs() < 1e-6);

        // 横边适应：宽度贴满 → 缩放 = 1236/1000 = 1.236，高 = 2472（超出画布）
        let scale_w = VIEW_W as f32 / 1000.0;
        assert!((2000.0 * scale_w - 2472.0).abs() < 1.5, "横边适应高度推算");
    }

    /// 竖边适应对「比屏幕高瘦」的页应该整页可见（宽 <= 画布宽）。
    ///
    /// 这是漫画单页的典型情况：比例 0.5 比屏幕的 0.75 更瘦，
    /// 按高度缩放后宽度自然装得下，整页一次看完不用裁也不用滚。
    #[test]
    fn 竖边适应能装下高瘦页() {
        let w = 1000.0;
        let h = 2000.0; // 比例 0.5
        let scale = VIEW_H as f32 / h;
        let dst_w = (w * scale).round() as usize;
        assert!(dst_w <= VIEW_W, "竖边适应后宽度 {dst_w} 应 <= {VIEW_W}");

        // 反过来说，屏幕更「宽」的情况下竖边适应会留白而不是裁切
        let dst_h = (h * scale).round() as usize;
        assert_eq!(dst_h, VIEW_H, "高度应正好贴满");
    }

    /// 长条漫的步长换算。
    #[test]
    fn 长条漫步长() {
        // 宽 800 的长条，按宽度缩到 1236 → scale = 1.545
        // 一屏要 1648 像素高 → 需要 1648/1.545 ≈ 1067 个图像像素
        let strip = solid(800, 20000);
        let step = strip_page_step(&strip);
        let expected = (VIEW_H as f32 / (VIEW_W as f32 / 800.0)).round() as usize;
        assert_eq!(step, expected);
        assert!(step < 20000, "步长应远小于整条高度");
    }

    /// 长条漫判定。
    #[test]
    fn 长条判定() {
        assert!(!is_long_strip(&solid(1000, 1500)), "1.5 倍不算长条");
        assert!(is_long_strip(&solid(1000, 3000)), "3 倍算长条");
    }

    /// ⭐ **长条漫切屏绝不能越界写**（真机崩过两次）。
    ///
    /// `slice_to_canvas` 里的拷贝是直接写 `dst[di..di+4]`，没有越界保护。
    /// 而 `dst_h = round(step × scale)` 会因为 `step` 四舍五入而**比画布高 1~2 行**：
    ///
    /// | 图宽 | scale | step | step×scale | dst_h |
    /// |---|---|---|---|---|
    /// | 800 | 1.5450 | 1067 | 1648.515 | **1649** |
    /// | 200 | 6.1800 | 267  | 1650.06  | **1650** |
    ///
    /// 真机日志：
    /// ```
    /// thread '<unnamed>' panicked at src/render.rs:448:20:
    /// range end index 8147716 out of range for slice of length 8147712
    /// Aborted → exit code 134
    /// ```
    /// 8147712 = 1236×1648×4（画布），8147716 正好是「多写一行」的下一个像素。
    ///
    /// 200..1600 之间有 42 个图宽档会触发 —— 所以这不是某个畸形图的特例，
    /// 而是**普通的 800px 宽长条漫就会踩**。
    #[test]
    fn 长条漫切屏不越界() {
        // 覆盖会崩的那些宽度档 + 几个常见档
        let widths = [
            200u32, 215, 245, 275, 305, 335, 365, 400, 480, 600, 700, 750, 790, 800, 810, 850,
            900, 1000, 1100, 1200, 1236, 1400, 1600,
        ];
        for w in widths {
            // 高度取「一屏多一点」，既够切又不用分配太大的缓冲
            let step = strip_page_step(&solid(w, 10));
            let h = (step + 10) as u32;
            let strip = solid(w, h);

            let (canvas, y, step2) = slice_to_canvas(&strip, 0);
            assert_eq!(canvas.width(), VIEW_W as u32, "图宽 {w}：画布宽度不对");
            assert_eq!(canvas.height(), VIEW_H as u32, "图宽 {w}：画布高度不对");
            assert!(y + step2 <= h as usize, "图宽 {w}：起始位置夹取后仍越界");

            // 再从末尾切一次（`y0` 会夹到底边对齐那一屏）
            let (canvas2, y2, _) = slice_to_canvas(&strip, h as usize);
            assert_eq!(canvas2.width(), VIEW_W as u32);
            assert!(y2 + step2 <= h as usize, "图宽 {w}：末尾那一屏越界");
        }
    }

    /// 解码要带回**缩放前**的原始尺寸 —— 禁漫解密靠它做分段边界映射。
    #[test]
    fn 解码带回原始尺寸() {
        use image::{ImageEncoder, RgbaImage};
        let img = RgbaImage::from_pixel(40, 30, image::Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(img.as_raw(), 40, 30, image::ExtendedColorType::Rgba8)
            .unwrap();

        let d = decode_meta(&bytes).unwrap();
        assert_eq!((d.orig_w, d.orig_h), (40, 30), "原始尺寸不对");
        assert_eq!((d.buf.width(), d.buf.height()), (40, 30), "小图不该被缩");
    }

    /// **一次性对照**（默认忽略）：客户端缩图质量 vs 电脑服务端的 LANCZOS。
    ///
    /// 背景：用户反馈「用电脑服务端看，比内置的清楚」。
    ///
    /// 差别在两处：
    ///   * 电脑端 `jm_kindle_raw.py` 先拿 **LANCZOS** 把图缩到屏幕尺寸再发；
    ///   * 内置服务端发原图，客户端在 `fit_to_canvas` 里用**最近邻**缩。
    ///
    /// 这里拿一张真实页面，把「最近邻」和「LANCZOS」分别缩到同样尺寸，
    /// 和 LANCZOS 的结果比平均绝对差（MAD，越小越接近）。
    ///
    /// 跑法：
    ///   cargo test --bin kindle-manga 缩放质量 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn 缩放质量对比() {
        // 样本放在 `samples/`，不在仓库里（受版权保护的漫画页）。
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("py_decoded_0.webp");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("跳过：{} 不存在", path.display());
            return;
        };
        let img = image::load_from_memory(&bytes)
            .expect("解码样本失败")
            .to_rgba8();
        let (w, h) = img.dimensions();

        // 模拟 FitHeight：高度贴满 VIEW_H
        let scale = VIEW_H as f32 / h as f32;
        let dst_w = ((w as f32) * scale).round().max(1.0) as u32;
        let dst_h = VIEW_H as u32;
        println!("源图 {w}×{h} → 画布内容 {dst_w}×{dst_h}（缩放 {scale:.3}）");

        let lanczos = image::imageops::resize(&img, dst_w, dst_h, FilterType::Lanczos3);
        let nearest = image::imageops::resize(&img, dst_w, dst_h, FilterType::Nearest);
        let triangle = image::imageops::resize(&img, dst_w, dst_h, FilterType::Triangle);

        let mad = |a: &image::RgbaImage, b: &image::RgbaImage| -> f64 {
            let mut sum = 0u64;
            for (pa, pb) in a.pixels().zip(b.pixels()) {
                for c in 0..3 {
                    sum += (pa.0[c] as i64 - pb.0[c] as i64).unsigned_abs();
                }
            }
            sum as f64 / (a.pixels().count() as f64 * 3.0)
        };

        println!("最近邻 vs LANCZOS：MAD = {:.2}", mad(&nearest, &lanczos));
        println!("三角滤波 vs LANCZOS：MAD = {:.2}", mad(&triangle, &lanczos));

        // ---- 实际管线：fit_to_canvas 的输出 vs LANCZOS 基准 ----
        let mut pb = PixelBuffer::new(w, h);
        pb.make_mut_bytes().copy_from_slice(img.as_raw());
        let canvas = fit_to_canvas(&pb, FitMode::FitHeight);

        let cw = canvas.width() as usize;
        let x_off = (VIEW_W as isize - dst_w as isize) / 2;
        let y_off = (VIEW_H as isize - dst_h as isize) / 2;

        let mut sum = 0u64;
        let mut n = 0u64;
        let bytes = canvas.as_bytes();
        for dy in 0..dst_h as usize {
            for dx in 0..dst_w as usize {
                let cx = x_off as usize + dx;
                let cy = y_off as usize + dy;
                let ci = (cy * cw + cx) * 4;
                let ri = (dy * dst_w as usize + dx) * 4;
                for c in 0..3 {
                    sum += (bytes[ci + c] as i64 - lanczos.as_raw()[ri + c] as i64).unsigned_abs();
                    n += 1;
                }
            }
        }
        println!("fit_to_canvas vs LANCZOS：MAD = {:.2}", sum as f64 / n as f64);

        // 耗时：Kindle 的 CPU 比这台机器慢得多，这里只能看个量级。
        // release 下跑 `cargo test --release ... 缩放质量 -- --ignored --nocapture`
        let t0 = std::time::Instant::now();
        const N: u32 = 10;
        for _ in 0..N {
            std::hint::black_box(fit_to_canvas(&pb, FitMode::FitHeight));
        }
        println!(
            "fit_to_canvas 单次 {:.2} ms（x86 release 基准）",
            t0.elapsed().as_secs_f64() * 1000.0 / N as f64
        );

        for (name, f) in [
            ("image Lanczos3", FilterType::Lanczos3),
            ("image Triangle", FilterType::Triangle),
            ("image CatmullRom", FilterType::CatmullRom),
        ] {
            let t = std::time::Instant::now();
            for _ in 0..N {
                std::hint::black_box(image::imageops::resize(&img, dst_w, dst_h, f));
            }
            println!(
                "{name} 单次 {:.2} ms",
                t.elapsed().as_secs_f64() * 1000.0 / N as f64
            );
        }

        // taps 构建本身要多久（每页一次）
        let t = std::time::Instant::now();
        for _ in 0..N {
            std::hint::black_box(Taps::for_scale(w as usize, dst_w as usize));
        }
        println!(
            "taps 构建 单次 {:.3} ms",
            t.elapsed().as_secs_f64() * 1000.0 / N as f64
        );

        // 尖锐度：相邻像素差的平方均值。最近邻会造成锯齿 → 数值偏高且发脏。
        let sharp = |a: &image::RgbaImage| -> f64 {
            let (w, h) = a.dimensions();
            let mut sum = 0f64;
            let mut n = 0f64;
            for y in 0..h - 1 {
                for x in 0..w - 1 {
                    let p = a.get_pixel(x, y).0[0] as f64;
                    let r = a.get_pixel(x + 1, y).0[0] as f64;
                    let d = a.get_pixel(x, y + 1).0[0] as f64;
                    sum += (p - r).powi(2) + (p - d).powi(2);
                    n += 2.0;
                }
            }
            sum / n
        };
        println!("尖锐度：最近邻 {:.1} / 三角 {:.1} / LANCZOS {:.1}",
            sharp(&nearest), sharp(&triangle), sharp(&lanczos));
    }

    /// 重采样：**缩小后的纯色图必须还是那个纯色**。
    ///
    /// 这条看着平淡，但它是权重归一化的地基：面积平均如果把权重算错
    /// （比如末尾补足那一项漏了），纯色图就会缩成「一边亮一边暗」。
    #[test]
    fn 重采样保持纯色() {
        for (sw, sh, dw, dh) in [
            (100u32, 100u32, 37u32, 37u32),
            (1280, 1818, 1160, 1648),
            (800, 600, 1200, 900), // 放大
        ] {
            let mut img = PixelBuffer::new(sw, sh);
            for px in img.make_mut_bytes().chunks_exact_mut(4) {
                px.copy_from_slice(&[123, 45, 200, 255]);
            }
            let out = resample(img.as_bytes(), sw as usize, sh as usize, dw as usize, dh as usize);
            for px in out.chunks_exact(4) {
                assert_eq!(
                    (px[0], px[1], px[2], px[3]),
                    (123, 45, 200, 255),
                    "{sw}×{sh} → {dw}×{dh} 纯色被改坏了"
                );
            }
        }
    }

    /// 缩小时**不能丢信息**：每行都是不同灰度的图缩下去之后，
    /// 结果必须仍然单调（面积平均不会像最近邻那样跳变或漏掉一整行）。
    #[test]
    fn 缩小不丢行() {
        let (sw, sh) = (16usize, 64usize);
        let (dw, dh) = (8usize, 32usize);

        let mut img = PixelBuffer::new(sw as u32, sh as u32);
        for y in 0..sh {
            let v = (y * 255 / (sh - 1)) as u8;
            let row = &mut img.make_mut_bytes()[y * sw * 4..(y + 1) * sw * 4];
            for px in row.chunks_exact_mut(4) {
                px.copy_from_slice(&[v, v, v, 255]);
            }
        }

        let out = resample(img.as_bytes(), sw, sh, dw, dh);
        let rows: Vec<u8> = (0..dh).map(|y| out[y * dw * 4]).collect();

        for w in rows.windows(2) {
            assert!(w[0] <= w[1], "缩小后行序反了：{} > {}", w[0], w[1]);
        }
        // 最近邻会「隔一个取一个」，首尾值会明显跳；面积平均应贴近线性渐变
        assert!(
            rows[0] < 12 && rows[dh - 1] > 243,
            "首尾灰度不对：{}..{}",
            rows[0],
            rows[dh - 1]
        );
    }

    /// 放大时**不能出方块**：双线性会让相邻像素之间平滑过渡，
    /// 而最近邻只会复制。拿一行 0/255 交替的图放大 4 倍，
    /// 中间必须出现非 0 非 255 的过渡值。
    #[test]
    fn 放大有过渡不出方块() {
        let (sw, sh) = (8usize, 2usize);
        let dw = 32usize;

        let mut img = PixelBuffer::new(sw as u32, sh as u32);
        for y in 0..sh {
            let row = &mut img.make_mut_bytes()[y * sw * 4..(y + 1) * sw * 4];
            for (x, px) in row.chunks_exact_mut(4).enumerate() {
                let v = if x % 2 == 0 { 0u8 } else { 255 };
                px.copy_from_slice(&[v, v, v, 255]);
            }
        }

        let out = resample(img.as_bytes(), sw, sh, dw, sh);
        let vals: Vec<u8> = (0..dw).map(|x| out[x * 4]).collect();
        let mids = vals.iter().filter(|v| **v > 20 && **v < 235).count();
        assert!(mids > 0, "放大后没有过渡值（只有 0 和 255），说明还是最近邻：{vals:?}");
    }

    /// 缩放后的画布必须**严格是屏幕尺寸**，且内容居中、四周补白。
    #[test]
    fn 画布尺寸与留白() {
        let img = solid(500, 1000); // 竖图，按高度贴满 → 左右留白
        let canvas = fit_to_canvas(&img, FitMode::FitHeight);
        assert_eq!(canvas.width(), VIEW_W as u32);
        assert_eq!(canvas.height(), VIEW_H as u32);

        let bytes = canvas.as_bytes();
        // 左上角一定是白的（图比屏幕窄，两侧留白）
        assert_eq!(bytes[0], 0xff, "左侧留白不是白");
        // 中间一行中点不应该全是白（那里是图）
        let mid = (VIEW_H / 2) * VIEW_W * 4 + (VIEW_W / 2) * 4;
        assert_eq!(bytes[mid], 0x80, "图中心像素不对");
    }

    /// 空图不该 panic。
    #[test]
    fn 空图安全() {
        let empty = PixelBuffer::new(0, 0);
        let c = fit_to_canvas(&empty, FitMode::FitHeight);
        assert_eq!(c.width(), VIEW_W as u32);
        let (c2, y, _) = slice_to_canvas(&empty, 0);
        assert_eq!(c2.width(), VIEW_W as u32);
        assert_eq!(y, 0);
    }

    /// 适配模式切换应该是「来回」的。
    #[test]
    fn 模式切换() {
        assert_eq!(FitMode::default(), FitMode::FitHeight, "默认应为竖边适应");
        assert_eq!(FitMode::FitHeight.toggled(), FitMode::FitWidth);
        assert_eq!(FitMode::FitWidth.toggled(), FitMode::FitHeight);
        assert_eq!(FitMode::FitHeight.label(), "竖边适应");
        assert_eq!(FitMode::FitWidth.label(), "横边适应");
    }
}

//! 禁漫图片的「横条重排」解密 —— **从服务端挪到了客户端**。
//!
//! 新架构（见 `API-CONTRACT.md`）里服务端只下载、**不解码**，
//! 原样把 webp 发出来；所以解密这一份活必须由客户端做。
//!
//! 算法与 `jmcomic` 的 `JmImageTool.decode_and_save` / `get_num` 完全一致
//! （`C:\...\site-packages\jmcomic\jm_toolkit.py`）。分两步：
//!
//! 1. [`segmentation_num`]：按 `scramble_id` + 本子 ID + 原始文件名算出切了几段；
//! 2. [`descramble_resize`]：把切乱的行块按几何位置拼回去，**同时**缩到目标尺寸。
//!
//! # ⚠️ 解密必须在**全分辨率**下做（踩过一次，用户看得见）
//!
//! 第一版是「先缩到 2.5 M 像素预算，再按比例把行块拼回去」。看起来能省内存，
//! 实际上会在**每一块的分界处留下一条横向细线**。
//!
//! 原因：降采样是把相邻几行**加权平均**成一行。分块边界两侧的行一旦被平均在
//! 一起，这一行就同时含有两个块的颜色，重排之后无论放到哪儿都是错的 ——
//! 表现就是一条条横贯画面的细线。
//!
//! 实测（缓存里的真实加密原图 2150×3035，`num=12`，缩到 2.5 M 像素）：
//!
//! ```text
//! 正确顺序（先解密后缩） vs 现状（先缩后解密）逐行平均差：
//!   全图平均 0.018
//!   11 处分块边界：0.154 0.181 0.187 0.162 0.218 0.137 0.162 0.376 0.262 0.172 0.234
//! ```
//!
//! 边界处的差异是平均值的 **8~20 倍**，而且恰好是 `num-1` 条 —— 和用户说的
//! 「好几条横着的细线」完全对上。缓存里 **26%** 的图超过 2.5 M 像素，
//! 也就是它们全都有这个问题。
//!
//! 所以现在：**解码出全分辨率 → 按块拼接并同时缩放**（`descramble_resize`）。
//! 缩放时也是**一块一块单独缩**，块与块之间绝不混合。
//!
//! # 分段数为什么必须由服务端告诉客户端
//!
//! 分段数**逐页不同**（实测同一章 32 页里出现了 2/4/6/8/10/12/14 六种值），
//! 它取决于 `md5("{aid}{文件名}")` 的最后一个十六进制字符。而客户端拿到的
//! 图片地址是 `/img/<photo_id>/<page>`，**看不到原始文件名**。所以服务端
//! 必须给足信息，按优先级支持两种写法：
//!
//! | 服务端字段 | 客户端行为 |
//! |---|---|
//! | 每页 `seg`（**推荐**） | 直接用，最省事也最不会错 |
//! | 每页 `filename` + 顶层 `scramble_id` | 客户端用 md5 自己算 |
//! | 只有顶层 `scramble_id` | 退化成「文件名 = 5 位页号」（实测源站都是这个命名） |
//!
//! 三者都没有 → 返回 0 → **不解密**（旧版 Python 服务端发的是已解密图，
//! 这条兼容路径保证换回旧服务端也不会把好图切坏）。

use crate::md5::md5_hex;
use crate::render::{self, FullImage, PixelBuffer};

/// 禁漫改过两次切割算法，这两个是分界点（和 jmcomic 的 `JmMagicConstants` 一致）。
pub const SCRAMBLE_268850: u64 = 268850;
/// 2023-02-08 起改成更细的切割（`x = 8`）。
pub const SCRAMBLE_421926: u64 = 421926;

/// 一页要切成几段。
///
/// * `aid < scramble_id` —— 这一本不加密，0；
/// * `aid < 268850` —— 固定 10 段；
/// * 之后按 `md5("{aid}{filename}")` 的最后一位算，偶数段 2..20。
///
/// `aid` 是**章节（photo）ID**，不是本子（album）ID —— jmcomic 里
/// `JmImageDetail.aid` 存的就是 `photo_id`（见 `jm_entity.py` 的 `of()`）。
pub fn segmentation_num(scramble_id: u64, aid: u64, filename: &str) -> u32 {
    if aid < scramble_id {
        return 0;
    }
    if aid < SCRAMBLE_268850 {
        return 10;
    }
    let x = if aid < SCRAMBLE_421926 { 10 } else { 8 };
    let hash = md5_hex(format!("{aid}{filename}").as_bytes());
    let last = hash.as_bytes().last().copied().unwrap_or(b'0') as u32;
    (last % x) * 2 + 2
}

/// 把切乱的行块拼回去，**同时**缩到 `dst_w × dst_h`。
///
/// 输入必须是**全分辨率**的图（`render::decode_full` 的输出）。
///
/// # 为什么不先缩再拼
///
/// 见文件头。简单说：降采样会混合相邻行，一旦混在分块边界上，重排后就是
/// 一条横向细线。
///
/// # 做法：只建一张「行号映射表」，重采样时顺手用
///
/// 因为解密重排的是**整行**，所以不需要真的把图搬一遍 —— 只要在重采样
/// 取源行的时候，把「排序后第 y 行」换成「它在乱序图里的第几行」即可：
///
/// ```text
/// 排序后的行 y  ∈ 第 i 块  →  乱序图里的行 = s0 + (y - d0)
/// ```
///
/// 这样得到的结果和「先把整张图重排、再整体缩放」**逐像素完全一致**
/// （用的是同一个重采样器、同一组权重），但内存里只多了一张 `h` 个
/// `u32` 的映射表（3000 行 = 12 KB），而不是一整份全分辨率图。
///
/// 关键点是：**缩放发生在全分辨率的重排语义之上**，所以降采样永远不会把
/// 两个块的行平均到一起 —— 那条横向细线就不会出现。
pub fn descramble_resize(src: &FullImage, num: u32, dst_w: u32, dst_h: u32) -> PixelBuffer {
    let w = src.w as usize;
    let h = src.h as usize;
    let dst_w = dst_w.max(1) as usize;
    let dst_h = dst_h.max(1) as usize;
    let num = num as usize;

    // 不用解密 / 尺寸异常：直接整体缩下去
    if num <= 1 || w == 0 || h == 0 || h < num || src.data.len() < w * h * 4 {
        return render::shrink_to(src, dst_w as u32, dst_h as u32);
    }

    let piece = h / num;
    let over = h % num;

    // 排序后的行号 → 乱序图里的行号
    let mut row_of = vec![0u32; h];
    for i in 0..num {
        let d0 = piece * i + if i == 0 { 0 } else { over };
        let d1 = if i + 1 == num { h } else { piece * (i + 1) + over };
        let s0 = h - piece * (i + 1) - over;
        for y in d0..d1 {
            row_of[y] = (s0 + (y - d0)) as u32;
        }
    }

    let mut buf = PixelBuffer::new(dst_w as u32, dst_h as u32);
    render::resample_mapped_into(
        buf.make_mut_bytes(),
        &src.data,
        w,
        h,
        dst_w,
        dst_h,
        |y| row_of.get(y).copied().unwrap_or(0) as usize,
    );
    buf
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一张「每行一个灰度 = 行号」的图，拼回去之后行号必须恢复成递增。
    fn ramp(w: u32, h: u32) -> PixelBuffer {
        let mut b = PixelBuffer::new(w, h);
        for (y, row) in b.make_mut_bytes().chunks_exact_mut(w as usize * 4).enumerate() {
            for px in row.chunks_exact_mut(4) {
                let v = (y % 251) as u8; // 251 是质数，避免取模后单调性被掩盖
                px[0] = v;
                px[1] = v;
                px[2] = v;
                px[3] = 255;
            }
        }
        b
    }

    /// 逆运算：把「已经排好的图」切乱，得到网上那种原始图。
    /// 这样测试不依赖外部数据，而且和 jmcomic 的映射严格互逆。
    fn scramble(img: &PixelBuffer, num: u32) -> PixelBuffer {
        let w = img.width() as usize;
        let h = img.height() as usize;
        let num = num as usize;
        let piece = h / num;
        let over = h % num;
        let row_bytes = w * 4;

        let mut out = PixelBuffer::new(w as u32, h as u32);
        let src = img.as_bytes();
        let dst = out.make_mut_bytes();

        for i in 0..num {
            let dst_start0 = piece * i + if i == 0 { 0 } else { over };
            let len = piece + if i == 0 { over } else { 0 };
            // 逆映射：原图（已排好）的 dst_start0 那一段，放回乱序图的 src_start0
            let src_start0 = h - piece * (i + 1) - over;
            let s = dst_start0 * row_bytes;
            let d = src_start0 * row_bytes;
            dst[d..d + len * row_bytes].copy_from_slice(&src[s..s + len * row_bytes]);
        }
        out
    }

    fn rows(img: &PixelBuffer) -> Vec<u8> {
        img.as_bytes()
            .chunks_exact(4)
            .step_by(img.width() as usize)
            .map(|p| p[0])
            .collect()
    }

    /// `PixelBuffer` → `FullImage`（测试里两边尺寸一样，纯搬运）。
    fn full_of(buf: &PixelBuffer) -> FullImage {
        FullImage::from_rgba(buf.as_bytes().to_vec(), buf.width(), buf.height())
    }

    /// 造一张「每一块是一个纯色」的图（**已经排好序**的版本）。
    ///
    /// 用来验证「块与块之间不会混合」：如果重采样跨了块边界，那一行的值
    /// 就会介于两个块的灰度之间，和任何一块都对不上。
    fn banded(w: u32, h: u32, num: u32) -> (PixelBuffer, Vec<u8>) {
        let mut b = PixelBuffer::new(w, h);
        let piece = h / num;
        let over = h % num;
        let mut colors = Vec::new();
        for i in 0..num {
            let v = (30 + i * 40) as u8; // 30, 70, 110 … 各不相同
            colors.push(v);
            let from = (piece * i + if i == 0 { 0 } else { over }) as usize;
            let len = (piece + if i == 0 { over } else { 0 }) as usize;
            for y in from..from + len {
                for px in b.make_mut_bytes()[y * w as usize * 4..(y + 1) * w as usize * 4]
                    .chunks_exact_mut(4)
                {
                    px.copy_from_slice(&[v, v, v, 255]);
                }
            }
        }
        (b, colors)
    }

    #[test]
    fn 分段数_与_python_一致() {
        // 期望值由 jmcomic 的 get_num 跑出来（纯算法，不联网）
        let cases = [
            (220980, 1475681, "00001", 10),
            (220980, 1475681, "00002", 8),
            (220980, 1475681, "00015", 10),
            (220980, 1475681, "00017", 6),
            (220980, 1475681, "00019", 14),
            (220980, 1475681, "00032", 4),
            // 未加密 / 老算法 / x=10 / x=8 四种分支
            (220980, 5888, "00001", 0),
            (220980, 230000, "00001", 10),
            (220980, 268850, "00001", 6),
            (220980, 300000, "00001", 16),
            (220980, 421926, "00001", 14),
            (220980, 500000, "abc", 12),
        ];
        for (sid, aid, name, want) in cases {
            assert_eq!(
                segmentation_num(sid, aid, name),
                want,
                "scramble={sid} aid={aid} 文件={name}"
            );
        }
    }

    /// 不缩放时，解密必须**逐行还原**。
    #[test]
    fn 解密还原() {
        for (h, num) in [(100u32, 5u32), (103, 5), (120, 7), (99, 10), (64, 2)] {
            let img = ramp(3, h);
            let scrambled = scramble(&img, num);
            let back = descramble_resize(&full_of(&scrambled), num, 3, h);
            assert_eq!(rows(&back), rows(&img), "h={h} num={num} 没还原");
        }
    }

    #[test]
    fn 不用解密时原样返回() {
        let img = ramp(4, 40);
        for num in [0, 1] {
            let out = descramble_resize(&full_of(&img), num, 4, 40);
            assert_eq!(rows(&out), rows(&img), "num={num} 不该改动图像");
        }
    }

    #[test]
    fn 空图安全() {
        let empty = FullImage::from_rgba(Vec::new(), 0, 0);
        let out = descramble_resize(&empty, 5, 4, 4);
        assert_eq!(out.width(), 4, "空图也要给出合法画布");
    }

    /// ⭐ **回归测试：缩放时块与块之间绝对不能混合。**
    ///
    /// 这就是用户报的「图片上有好几条横着的细线」。
    ///
    /// 做法：每一块涂成**纯色**（各不相同），再打乱。解密 + 缩放之后，
    /// 输出里每一行都必须**恰好等于**某一块的灰度 —— 一旦某个块边界两侧的
    /// 源行被平均在一起，就会出现介于两者之间的值，那就是接缝。
    #[test]
    fn 缩放时块边界不混合() {
        let (h, num, dst_h) = (600u32, 6u32, 300u32);
        let (sorted, colors) = banded(4, h, num);
        let scrambled = scramble(&sorted, num);

        let out = descramble_resize(&full_of(&scrambled), num, 4, dst_h);
        assert_eq!(out.height(), dst_h);

        let r = rows(&out);
        let bad: Vec<(usize, u8)> = r
            .iter()
            .enumerate()
            .filter(|(_, v)| !colors.contains(v))
            .map(|(i, v)| (i, *v))
            .collect();
        assert!(
            bad.is_empty(),
            "出现了块与块混合的颜色（这就是横向细线）：{bad:?}"
        );

        // 而且顺序必须是 colors[0], colors[1], …（从上到下）
        let mut idx = 0usize;
        for v in &r {
            while idx + 1 < colors.len() && *v != colors[idx] {
                idx += 1;
            }
            assert_eq!(*v, colors[idx], "颜色顺序乱了");
        }
    }

    /// 只在测试里用：把排好序的全分辨率图**只按块重排**，不缩放。
    fn permute_full(src: &FullImage, num: u32) -> FullImage {
        let w = src.w as usize;
        let h = src.h as usize;
        let num = num as usize;
        let piece = h / num;
        let over = h % num;
        let rb = w * 4;
        let mut out = vec![0u8; h * rb];
        for i in 0..num {
            let d0 = piece * i + if i == 0 { 0 } else { over };
            let len = piece + if i == 0 { over } else { 0 };
            let s0 = h - piece * (i + 1) - over;
            out[d0 * rb..(d0 + len) * rb].copy_from_slice(&src.data[s0 * rb..(s0 + len) * rb]);
        }
        FullImage::from_rgba(out, src.w, src.h)
    }

    /// **旧实现**（先缩后解密），只用来在下面的对照里当反面教材。
    fn old_way(src: &FullImage, num: u32, tw: u32, th: u32) -> PixelBuffer {
        let small = render::shrink_to(src, tw, th);
        let (w, h) = (small.width() as usize, small.height() as usize);
        let num = num as usize;
        let orig_h = src.h as usize;
        let piece0 = orig_h / num;
        let over0 = orig_h % num;
        let mut out = PixelBuffer::new(w as u32, h as u32);
        let rb = w * 4;
        let map = |y: usize| (y * h / orig_h).min(h);
        {
            let s = small.as_bytes();
            let d = out.make_mut_bytes();
            for i in 0..num {
                let d0 = map(piece0 * i + if i == 0 { 0 } else { over0 });
                let d1 = if i + 1 == num {
                    h
                } else {
                    map(piece0 * (i + 1) + over0)
                };
                if d1 <= d0 {
                    continue;
                }
                let ss = orig_h - piece0 * (i + 1) - over0;
                let slen0 = piece0 + if i == 0 { over0 } else { 0 };
                let s0 = map(ss);
                let s1 = map(ss + slen0).max(s0 + 1).min(h);
                let (slen, dlen) = (s1 - s0, d1 - d0);
                for r in 0..dlen {
                    let sy = (s0 + r * slen / dlen).min(h - 1);
                    d[(d0 + r) * rb..(d0 + r + 1) * rb].copy_from_slice(&s[sy * rb..(sy + 1) * rb]);
                }
            }
        }
        out
    }

    /// 逐行平均差，返回 `(全图平均, 块边界处的最大比值)`。
    fn seam_score(got: &PixelBuffer, want: &PixelBuffer, num: usize, orig_h: usize) -> (f64, f64) {
        let w = got.width() as usize;
        let h = got.height() as usize;
        let row_mad = |y: usize| -> f64 {
            let a = got.as_bytes();
            let b = want.as_bytes();
            let mut sum = 0u64;
            for x in 0..w {
                let i = (y * w + x) * 4;
                for c in 0..3 {
                    sum += (a[i + c] as i64 - b[i + c] as i64).unsigned_abs();
                }
            }
            sum as f64 / (w * 3) as f64
        };

        let all: Vec<f64> = (0..h).map(row_mad).collect();
        let avg = all.iter().sum::<f64>() / h as f64;

        let piece0 = orig_h / num;
        let over0 = orig_h % num;
        let mut worst: f64 = 0.0;
        for i in 1..num {
            let d = piece0 * i + over0;
            let y = (d * h / orig_h).min(h - 1);
            for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                if avg > 1e-9 {
                    worst = worst.max(all[yy] / avg);
                }
            }
        }
        (avg, worst)
    }

    /// ⭐⭐ **真实图片对照**（默认忽略）：证明「先缩后解密」会出接缝、
    /// 新的「按块解密 + 缩放」不会。
    ///
    /// 样本：一张真实加密原图（2150×3035，`num=12`），放在
    /// `samples/encrypted.webp`（不在仓库里 —— 有版权）。
    ///
    /// 跑法：
    /// ```text
    /// cargo test --release --bin kindle-manga 真实图接缝 -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn 真实图接缝对比() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples")
            .join("encrypted.webp");
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("跳过：{} 不存在", path.display());
            return;
        };

        let full = crate::render::decode_full(&bytes).unwrap();
        let num = segmentation_num(220980, 1211540, "0001");
        println!("样本 {}×{}，num={num}", full.w, full.h);
        let (tw, th) = crate::render::plan_size(full.w, full.h, false);
        println!("目标 {tw}×{th}");

        // 参考：**正确顺序**（全分辨率重排 → 整体缩放）。这就是 jmcomic 的结果。
        let want = render::shrink_to(&permute_full(&full, num), tw, th);

        let new = descramble_resize(&full, num, tw, th);
        let old = old_way(&full, num, tw, th);

        let (navg, nworst) = seam_score(&new, &want, num as usize, full.h as usize);
        let (oavg, oworst) = seam_score(&old, &want, num as usize, full.h as usize);

        println!();
        println!("  新（按块解密+缩）：全图平均差 {navg:.4}，块边界最差 {nworst:.1}× 平均");
        println!("  旧（先缩后解密）  ：全图平均差 {oavg:.4}，块边界最差 {oworst:.1}× 平均");
        println!();

        assert!(nworst < 3.0, "新算法在块边界仍有尖峰：{nworst:.1}×");
        assert!(
            oworst > 5.0,
            "旧算法居然没有接缝？说明这组对照没测到东西（{oworst:.1}×）"
        );
        assert!(navg < oavg, "新算法的整体误差应该更小");
    }

    /// 读进程的内存高水位（Linux 专用；只有这个一次性测量会用到）。
    fn vm_hwm_mb() -> f64 {
        let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
            return -1.0;
        };
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                let kb: f64 = rest
                    .trim()
                    .trim_end_matches(" kB")
                    .trim()
                    .parse()
                    .unwrap_or(0.0);
                return kb / 1024.0;
            }
        }
        -1.0
    }

    /// **一次性测量**（默认忽略）：最坏情况下的内存峰值。
    ///
    /// 造一张 2150×7516（= 16.16 M 像素，RGBA 64.6 MB）——这正是真机上
    /// `memory allocation of 64637600 bytes failed` 崩掉的那一页。
    ///
    /// 跑法：
    /// ```text
    /// cargo test --release --bin kindle-manga 内存峰值 -- --ignored --nocapture --test-threads=1
    /// ```
    #[test]
    #[ignore]
    fn 内存峰值_最坏情况() {
        let (w, h) = (2150u32, 7516u32);
        let (tw, th) = crate::render::plan_size(w, h, true);
        println!("源 {w}×{h}（RGBA {:.1} MB）→ 目标 {tw}×{th}", (w as f64 * h as f64 * 4.0) / 1e6);
        println!("起始高水位 {:.1} MB", vm_hwm_mb());

        let full = FullImage::from_rgba(vec![180u8; w as usize * h as usize * 4], w, h);
        println!("造完全分辨率图：高水位 {:.1} MB（含测试进程本身）", vm_hwm_mb());

        let out = descramble_resize(&full, 12, tw, th);
        println!("解密+缩放完：高水位 {:.1} MB，输出 {}×{}", vm_hwm_mb(), out.width(), out.height());

        assert_eq!((out.width(), out.height()), (tw, th));
        let hwm = vm_hwm_mb();
        if hwm > 0.0 {
            println!("净峰值（减去源图 {:.1} MB）= {:.1} MB", (w as f64 * h as f64 * 4.0) / 1e6,
                     hwm - (w as f64 * h as f64 * 4.0) / 1e6);
            assert!(hwm < 250.0, "峰值 {hwm:.0} MB 太高了");
        }
    }

    /// 缩放之后的整体顺序仍然必须是从上到下递增（不能倒序、不能有白缝）。
    #[test]
    fn 缩放后仍按顺序拼回() {
        let (h, num) = (200u32, 5u32);
        let img = ramp(1, h);
        let scrambled = scramble(&img, num);

        let out = descramble_resize(&full_of(&scrambled), num, 1, 100);
        assert_eq!(out.height(), 100);
        let r = rows(&out);
        assert!(r[0] < 5, "第一行该来自顶部（实际 {}）", r[0]);
        assert!(r[r.len() - 1] > 190, "最后一行该来自底部（实际 {}）", r[r.len() - 1]);
        for w in r.windows(2) {
            assert!(w[0] <= w[1], "出现倒序：{} > {}", w[0], w[1]);
        }
    }

    /// **一次性真实样本对照**（默认忽略，验证时手动跑）：
    ///
    /// 样本放在 `samples/`（不在仓库里 —— 是受版权保护的漫画页，
    /// 需要自己准备两份：`raw_00.webp` 是从源站下载的**未解密**原图，
    /// `py_decoded_0.webp` 是用 jmcomic 正常解密的结果）。
    ///
    /// 两份都是有损 webp，所以比亮度平均绝对差，判定 **MAD < 3**。
    /// 没有样本就跳过，不算失败。
    #[test]
    #[ignore]
    fn 真实样本_与jmcomic一致() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
        let (Ok(raw), Ok(reference)) = (
            std::fs::read(dir.join("raw_00.webp")),
            std::fs::read(dir.join("py_decoded_0.webp")),
        ) else {
            eprintln!("跳过：{} 里没有真实样本", dir.display());
            return;
        };

        let full = crate::render::decode_full(&raw).unwrap();
        let num = segmentation_num(220980, 1475681, "00001");
        assert_eq!(num, 10, "第 1 页该切成 10 段");
        // 这个样本 1280×1818 = 2.33 M 像素，在 2.5 M 预算内 → 不缩放，
        // 所以是「解密还原」的逐行对照。
        let out = descramble_resize(&full, num, full.w, full.h);

        let r = crate::render::decode_full(&reference).unwrap();
        assert_eq!(
            (out.width(), out.height()),
            (r.w, r.h),
            "尺寸对不上"
        );

        let a = out.as_bytes();
        let b = &r.data;
        let mut sum = 0u64;
        let mut n = 0u64;
        for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            let la = (77 * pa[0] as u32 + 150 * pa[1] as u32 + 29 * pa[2] as u32) >> 8;
            let lb = (77 * pb[0] as u32 + 150 * pb[1] as u32 + 29 * pb[2] as u32) >> 8;
            sum += (la as i64 - lb as i64).unsigned_abs();
            n += 1;
        }
        let mad = sum as f64 / n as f64;
        println!("MAD = {mad:.3}（判定阈值为 3）");
        assert!(mad < 3.0, "和 jmcomic 的结果差距过大：MAD={mad:.3}");
    }

}

//! 禁漫图片的「横条重排」解密 —— **从服务端挪到了客户端**。
//!
//! 新架构（见 `API-CONTRACT.md`）里服务端只下载、**不解码**，
//! 原样把 webp 发出来；所以解密这一份活必须由客户端做。
//!
//! 算法与 `jmcomic` 的 `JmImageTool.decode_and_save` / `get_num` 完全一致
//! （`C:\...\site-packages\jmcomic\jm_toolkit.py`）。分两步：
//!
//! 1. [`segmentation_num`]：按 `scramble_id` + 本子 ID + 原始文件名算出切了几段；
//! 2. [`descramble_scaled`]：把切乱的行块按几何位置拼回去。
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
use crate::render::PixelBuffer;

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

/// 把切乱的行块拼回去。
///
/// `orig_h` 是**缩放前**的图像高度，只在解码阶段被降采样过时才和
/// `buf.height()` 不同（见 `render::decode`）。按比例映射每一块的位置，
/// 避免「先缩后拼」时 `h % num` 变化导致接缝错位。
///
/// `num <= 1` 原样返回（克隆一份，调用方可以安全地继续用返回值）。
pub fn descramble_scaled(buf: &PixelBuffer, num: u32, orig_h: u32) -> PixelBuffer {
    let w = buf.width() as usize;
    let h = buf.height() as usize;
    let num = num as usize;

    if num <= 1 || w == 0 || h == 0 {
        return buf.clone();
    }

    let orig_h = if orig_h == 0 { h } else { orig_h as usize };
    if orig_h < num {
        return buf.clone();
    }

    // 原始图里的切割参数（和 jmcomic 一致）
    let piece0 = orig_h / num;
    let over0 = orig_h % num;

    let mut out = PixelBuffer::new(w as u32, h as u32);
    let row_bytes = w * 4;
    let src = buf.as_bytes();
    let dst = out.make_mut_bytes();

    // 映射到当前（可能已缩放）高度。`d1` 用下一块的目标起点，保证目标行
    // 首尾相接、不重不漏；源区间按 strip 内部线性插值取行。
    let map = |y0: usize| -> usize { (y0 * h / orig_h).min(h) };

    for i in 0..num {
        let dst_start0 = piece0 * i + if i == 0 { 0 } else { over0 };
        let d0 = map(dst_start0);
        let d1 = if i + 1 == num { h } else { map(piece0 * (i + 1) + over0) };
        if d1 <= d0 {
            continue;
        }

        let src_start0 = orig_h - piece0 * (i + 1) - over0;
        let src_len0 = piece0 + if i == 0 { over0 } else { 0 };
        let s0 = map(src_start0);
        let s1 = map(src_start0 + src_len0).max(s0 + 1).min(h);
        let slen = s1 - s0;
        let dlen = d1 - d0;

        for r in 0..dlen {
            // 块内线性取样：即使缩放让两侧行数差 1~2 行，也不会出现白缝
            let sy = (s0 + r * slen / dlen).min(h - 1);
            let so = sy * row_bytes;
            let dofs = (d0 + r) * row_bytes;
            dst[dofs..dofs + row_bytes].copy_from_slice(&src[so..so + row_bytes]);
        }
    }

    out
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

    #[test]
    fn 解密还原() {
        for (h, num) in [(100u32, 5u32), (103, 5), (120, 7), (99, 10), (64, 2)] {
            let img = ramp(3, h);
            let scrambled = scramble(&img, num);
            let back = descramble_scaled(&scrambled, num, h);
            assert_eq!(rows(&back), rows(&img), "h={h} num={num} 没还原");
        }
    }

    #[test]
    fn 不用解密时原样返回() {
        let img = ramp(4, 40);
        for num in [0, 1] {
            let out = descramble_scaled(&img, num, 40);
            assert_eq!(rows(&out), rows(&img), "num={num} 不该改动图像");
        }
    }

    #[test]
    fn 空图安全() {
        let empty = PixelBuffer::new(0, 0);
        let out = descramble_scaled(&empty, 5, 0);
        assert_eq!(out.width(), 0);
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

        let d = crate::render::decode_meta(&raw).unwrap();
        let num = segmentation_num(220980, 1475681, "00001");
        assert_eq!(num, 10, "第 1 页该切成 10 段");
        let out = descramble_scaled(&d.buf, num, d.orig_h);

        let r = crate::render::decode_meta(&reference).unwrap();
        assert_eq!(
            (out.width(), out.height()),
            (r.buf.width(), r.buf.height()),
            "尺寸对不上"
        );

        let a = out.as_bytes();
        let b = r.buf.as_bytes();
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

    /// 缩放过的高度：拼回来之后整体顺序仍必须是从上到下递增
    /// （块内按比例取样，允许个别行重复，但不允许出现白缝/倒序）。
    #[test]
    fn 缩放后仍按顺序拼回() {
        let h0 = 200u32;
        let num = 5u32;
        let img = ramp(1, h0);
        let scrambled = scramble(&img, num);

        // 模拟「解码时被降采样」：隔行取，得到 h=100 的图
        let h1 = 100u32;
        let mut small = PixelBuffer::new(1, h1);
        {
            let s = scrambled.as_bytes();
            let d = small.make_mut_bytes();
            for y in 0..h1 as usize {
                let sy = y * 2;
                d[y * 4..y * 4 + 4].copy_from_slice(&s[sy * 4..sy * 4 + 4]);
            }
        }

        let out = descramble_scaled(&small, num, h0);
        assert_eq!(out.height(), h1);
        let r = rows(&out);
        assert!(r[0] < 5, "第一行该来自顶部（实际 {}）", r[0]);
        assert!(r[r.len() - 1] > 180, "最后一行该来自底部（实际 {}）", r[r.len() - 1]);
        for w in r.windows(2) {
            assert!(w[0] <= w[1], "出现倒序：{} > {}", w[0], w[1]);
        }
    }
}

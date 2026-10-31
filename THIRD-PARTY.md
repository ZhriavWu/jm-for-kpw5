# 第三方组件与许可

本项目以 **GPL-3.0** 发布（见 [LICENSE](LICENSE)）。下面列出直接依赖的第三方组件。

## 主要依赖

| 组件 | 版本 | 许可 | 用途 |
|---|---|---|---|
| [Slint](https://slint.dev/) | 1.18 | **GPL-3.0** / 商业 | UI 框架。**这是本项目必须用 GPL-3.0 的原因** —— Slint 的开源版就是 GPL-3.0 |
| [slint-backend-kindle](https://github.com/sverrejb/slint-kindle-backend) | 0.7.1 | MIT OR Apache-2.0 | Kindle E Ink 平台后端（见下方 vendor 说明） |
| [image](https://github.com/image-rs/image) | 0.25 | MIT OR Apache-2.0 | webp/png/jpeg/gif 解码 |
| [serde](https://serde.rs/) / serde_json | 1.x | MIT OR Apache-2.0 | JSON 序列化 |
| [jmapi](https://github.com/laoin114514/jmapi) | v0.1.5 | MIT | 禁漫站点 API 客户端（Go） |

## vendor 的 fork

`vendor/slint-backend-kindle/` 是 `slint-backend-kindle` 0.7.1 的**一份副本**，
不是原样拷贝 —— 做了以下功能性修改：

1. 新增 `src/refresh.rs`：全屏刷新策略（`RefreshPolicy`）
2. `src/lib.rs` / `src/platform.rs` 接线，公开 `KindleBackend::full_refresh_policy()`

原因：上游把 `Framebuffer` 和 `refresh_full()` 设成了 `pub(crate)`，应用层拿不到
「立即全屏刷新」的入口；而 Kindle 的墨水残影只能靠定期全刷清掉。

升级上游时的重新套用步骤见 `vendor/slint-backend-kindle/PATCH.md`。
上游许可（MIT OR Apache-2.0）的原文保留在该目录下。

## server/jmapi/

`jmapi` 的副本，打了 3 个补丁（见 `server/jmapi/` 里的 `client.go` 中标注
`[本地补丁]` 的位置）。上游许可是 MIT，原文在 `server/jmapi/LICENSE`。

## 数据与字体

| 资源 | 来源 | 许可 |
|---|---|---|
| `ime/zh_pinyin_data.lua` | [KOReader](https://github.com/koreader/koreader) 的 `rime-pinyin-simp` 数据 | Apache-2.0 |
| `ime/pinyin.bin` | 由 `ime/gen_table.py` 从上面那份 Lua 生成 | 同上 |
| `fonts/WenQuanYiMicroHei-Subset.ttf` | 文泉驿微米黑（Ubuntu `fonts-wqy-microhei` 包），裁成单面并子集化 | Apache-2.0 |

字体和拼音表都是**构建期 `include_bytes!` 编进二进制**的，不是运行时加载的文件。

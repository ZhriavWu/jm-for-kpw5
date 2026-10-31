//! Kindle 漫画客户端。
//!
//! 数据全部来自 `jm_kindle_raw.py`（接口说明见 `src/api.rs`）。
//!
//! # 三级构建目标
//!
//! ```text
//! cargo run --features desktop                    桌面窗口，秒级迭代 UI
//! --features kindle --target armv7-...gnueabihf   动态链接，供 QEMU 模拟环境注入 shim
//! --features kindle --target armv7-...musleabihf  静态链接，部署到真机
//! ```
//!
//! 两个 kindle 构建走同一份源码，只差链接方式：模拟环境**必须**动态链接，
//! 否则 `LD_PRELOAD` 的帧缓冲/触摸 shim 注入不进去（静态二进制没有动态链接器）。
//!
//! # 线程模型
//!
//! ```text
//! 后台线程   独占全部状态，负责网络与解码；状态一变就把整屏 View 推给 UI
//! UI 线程    只负责把 View 画出来，把点击翻译成 Cmd 丢回去
//! ```
//!
//! 这样 UI 线程永远不会被网络阻塞，也不会出现「两边各存一份状态、然后对不上」。
//! 跨线程传图像传的是 `SharedPixelBuffer` 而不是 `slint::Image` —— 后者不是 `Send`。
//!
//! # 本地单章模式
//!
//! `--demo` / `--dir <目录>` / `MANGA_DIR=<目录>` 会让客户端完全不碰网络，
//! 直接拿本地图片当一章来读。模拟器的自检脚本（e2e / interact / pagewalk / jpeg）
//! 全靠这条路，所以它必须一直能用。

mod api;
mod descramble;
mod favorites;
mod ime;
mod md5;
mod recent;
mod render;

// 电源键与休眠由应用自己接管（原因见模块内注释）。
// 只在真机/模拟器的 ARM 构建里有意义：桌面预览没有这些 evdev 设备，
// 而且桌面跑的时候不该去写 /sys/power/state。
//
// `test` 时也编进来 —— 里面的字节解析和 ioctl 常量是最容易「写错了也不报错、
// 只是永远收不到事件」的部分，值得用单元测试钉住（测试在宿主 x86 上跑）。
#[cfg(any(all(feature = "kindle", target_arch = "arm"), test))]
mod power;

// 挂起前把用户的屏保图铺到屏幕上。
// powerd 原本负责这件事，但它要穿过框架画屏保，而框架被我们冻住了 ——
// 所以接管休眠之后，这一步得自己补上，否则息屏时屏幕上还停在最后一页漫画。
#[cfg(any(all(feature = "kindle", target_arch = "arm"), test))]
mod screensaver;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};

use render::{PixelBuffer, THUMB_MAX};

slint::include_modules!();

/// Kindle 后端要求打包一份字体：Kindle 系统既没有 fontconfig 也没有默认字体路径，
/// Slint 的软件渲染器在第一次字体回退查询时会直接 panic。
///
/// ⚠️ **字体必须覆盖中日文**，这不是「好看不好看」的问题，是能不能显示的问题。
///
/// 最初用的是 LiberationSans —— 它只有 2327 个码位，全是拉丁/希腊/西里尔，
/// **一个汉字都没有**。结果所有本子标题、章节名全是空白，界面看起来像坏了。
/// 而漫画标题恰恰全是中日文，所以这个字体是彻底不能用的。
///
/// 现在的字体是 WenQuanYi Micro Hei 裁出来的子集（见 `fonts/README.md`）：
/// 保留中日文 + 拉丁 + 标点，压到 3 MB。用它当**唯一**的默认字体 ——
/// 它本身也带拉丁字母和数字，所以不存在「回退到哪个字体」的不确定性。
#[cfg(all(feature = "kindle", target_arch = "arm"))]
static FONT: &[u8] = include_bytes!("../fonts/WenQuanYiMicroHei-Subset.ttf");

// ---------------------------------------------------------------- 全屏刷新

/// 全刷间隔的默认值：每 1 次刷新就全刷 —— 也就是「每次都全刷」。
///
/// 听着极端，但 Kindle 上局部刷新的收益（快、不闪）远不如**残影**带来的困扰，
/// 而漫画是大面积灰度图，残影最明显。想看快一点的局部刷新就把这个数调大。
const FULL_REFRESH_DEFAULT: u32 = 1;

/// 间隔上限。0 是「关掉」，所以 0..=999 就是全部合法取值。
const FULL_REFRESH_MAX: u32 = 999;

/// 数字小键盘最多能输几位。9 位够用：本子 ID 最长 7 位，页码和刷新间隔远到不了。
/// 设上限是为了防止输入框被无限长的数字撑爆（覆盖层里那个光标 x 是按位数算的）。
const KEYPAD_MAX_DIGITS: usize = 9;

// 和后端 vendor 里的上限必须一致，不一致就会「用户填了 600，界面显示 600，
// 实际按 999 卡」这种诡异的错位。编译期钉住。
#[cfg(all(feature = "kindle", target_arch = "arm"))]
const _: () = assert!(FULL_REFRESH_MAX == slint_backend_kindle::MAX_INTERVAL);

/// 「每 N 次刷新全刷一次」策略的句柄。
///
/// 桌面预览没有电子墨水屏，也就没有残影这回事 —— 那里它是个空壳，所有操作
/// 都不做任何事。这样上层代码不用到处写 `cfg`。
#[derive(Clone, Default)]
struct RefreshHandle {
    #[cfg(all(feature = "kindle", target_arch = "arm"))]
    policy: Option<std::sync::Arc<slint_backend_kindle::RefreshPolicy>>,
}

impl RefreshHandle {
    /// 这台设备上有没有真东西。桌面预览是 false —— 界面上要据此隐藏这一项。
    fn available(&self) -> bool {
        #[cfg(all(feature = "kindle", target_arch = "arm"))]
        {
            self.policy.is_some()
        }
        #[cfg(not(all(feature = "kindle", target_arch = "arm")))]
        {
            false
        }
    }

    /// 设置间隔。0 = 关掉自动全刷。跨线程安全（内部是原子量）。
    fn set_interval(&self, interval: u32) {
        #[cfg(all(feature = "kindle", target_arch = "arm"))]
        if let Some(p) = &self.policy {
            p.set_interval(interval);
        }
        #[cfg(not(all(feature = "kindle", target_arch = "arm")))]
        let _ = interval;
    }

    /// 下一次刷新无条件全刷一次。用户刚改完设置时让他立刻看到效果。
    fn force_next(&self) {
        #[cfg(all(feature = "kindle", target_arch = "arm"))]
        if let Some(p) = &self.policy {
            p.force_next_refresh();
        }
    }
}

#[cfg(all(feature = "kindle", target_arch = "arm"))]
fn install_backend() -> (&'static str, RefreshHandle) {
    let backend = slint_backend_kindle::install(FONT).expect("安装 Kindle E Ink 后端失败");
    let handle = RefreshHandle {
        policy: Some(backend.full_refresh_policy()),
    };
    ("Kindle E Ink", handle)
}

#[cfg(not(all(feature = "kindle", target_arch = "arm")))]
fn install_backend() -> (&'static str, RefreshHandle) {
    ("桌面预览", RefreshHandle::default())
}

// ---------------------------------------------------------------- 常量

/// `--demo` 模式生成多少页测试图。
const DEMO_PAGES: usize = 50;

/// 历史列表每屏几条。8 行 × 186px = 1488，正好铺满内容区。
const HISTORY_PER_PAGE: usize = 8;

/// 章节目录每屏几条。两列 × 8 行 = 16。
/// 详情页每屏几个章节。
///
/// 现在详情页顶部被「封面 + 标题 + 标签」占掉约 392px（见 ui/app.slint），
/// 底下剩 1104px 给目录，6 行 × 184px = 1104 正好铺满，两列就是 12 个。
const CHAPTERS_PER_PAGE: usize = 12;

/// 原图字节缓存留几页。服务端是现下载+转码的，翻回去时能省一次网络往返。
const RAW_CACHE_MAX: usize = 6;

/// 键盘三行字母。**和 `ui/app.slint` 里 `kb-row-1/2/3` 的默认值一致** ——
/// 那边只定义一份、按下的字符也直接从那边回调回来，这里再存一份是为了
/// 「符号 / 字母」切换后能切回来。
const KB_LETTERS: [&[&str]; 3] = [
    &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
    &["a", "s", "d", "f", "g", "h", "j", "k", "l"],
    &["z", "x", "c", "v", "b", "n", "m"],
];

/// 符号页。点「符号」键切到这里，再点一次切回字母。
///
/// ⚠️ v5 的键盘只画了字母三行，但**服务器地址是 IP、端口和点号组成的**，
/// 没有数字就填不了地址。所以符号页不是可选装饰，是必需的兜底。
/// 每行的键数（10 / 9 / 7）必须和字母页一模一样，否则按键的 x 会错位。
const KB_SYMBOLS: [&[&str]; 3] = [
    &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
    &["-", "/", ".", ":", "_", "@", "#", "?", "!"],
    &["+", "=", "*", "&", "%", "$", "~"],
];

/// 搜索页三列网格里，封面框的最大边长。
///
/// 格子 376 宽、左右各留 18 → 340；和 `ui/chrome.slint` 的 `CoverCell`
/// 默认值、以及那里「标题从 y=376 开始」的排版是绑定的，改一处要改两处。
const GRID_COVER: u32 = 340;

/// 搜索结果区的可用高度（逻辑像素）。
///
/// = 1648 − 搜索栏 176 − 排序栏 96 − 底栏 108 = 1268？不对 —— 结果区从 272
/// 一直铺到翻页条顶（1648−108−140=1400），所以是 **1400 − 272 = 1128**。
///
/// 键盘是**盖**在上面的，不占这段高度（规格第 9 条：覆盖优于重排）——
/// 打字时列表不会被挤上去，位置一个像素都不动。
const SEARCH_LIST_H: i32 = 1128;

/// 搜索结果的行高：把结果区均分给当前页的条数。
///
/// 为什么要均分：原来写死 141px（按 8 行设计），而服务端每页只给 5~6 条 ——
/// 于是底下空出一大片（用户反馈「太少了，底下会有空白」）。
/// 改成按条数均分之后，有几条就铺满几条。
///
/// 夹在 104..240：
///   * 下限 104 —— 再矮封面框（96px）就顶到行了，列表会显得挤；
///   * 上限 240 —— 只有一两条时别把行撑得比封面还夸张。
fn search_row_h(n: usize) -> i32 {
    if n == 0 {
        return 141; // 空列表（显示一句提示），高度无所谓，给个常规值
    }
    (SEARCH_LIST_H / n as i32).clamp(104, 240)
}

/// 候选格的字号：按**本页最长的那条**候选算，整行统一。
///
/// 候选格 204px 宽，去掉左右各 7px 内边距，可用约 190px。
/// 文泉驿里中日文是全角（一个字符 = 1em），所以直接按字数 × 字号估即可。
///
/// 设计稿 v6 给的是两档（「44px，5 字以上降到 36px」）。这里多加了 30 / 26 两档：
/// 那张拼音表里有 6 个字以上的词（`eryanghualiu` = 二氧化硫是 4 个字，
/// 但还有更长的），两档的话 6 个字会用 216px 撑破格子、被 `elide` 截成半截 ——
/// **候选词被截断比字号小一点更糟**，用户看不出那是什么词。
fn cand_font_size(max_len: usize) -> i32 {
    match max_len {
        0..=4 => 44,
        5 => 36,
        6 => 30,
        _ => 26,
    }
}

// 光标的位置**不再由这里计算** —— 原来有个 `caret_x()` 按字符估宽度
// （中日文 44 / ASCII 28 + 字距 3），但估出来的值和实际渲染步进对不上
// （算 31、实际约 26），误差随字数累积，字越多偏得越远。
//
// 现在光标是 `ui/app.slint` 里紧跟在 `Text` 后面的一个格子，
// 由 `HorizontalLayout` 排版 —— 同一个字体引擎算出来的位置，结构上不会漂。
// 改输入框的**字号或字距**时，记得文字和光标要一起改（它们是同一个布局里的兄弟）。

// ---------------------------------------------------------------- 本地页源

/// 本地单章模式下的一页。
enum LocalPage {
    /// 本地生成一张测试图（不依赖任何文件）
    Demo(usize),
    /// 磁盘上的一张图
    File(PathBuf),
}

/// 解析命令行 / 环境变量，决定要不要走本地单章模式。
fn local_pages() -> Option<Vec<LocalPage>> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--demo") {
        return Some((0..DEMO_PAGES).map(LocalPage::Demo).collect());
    }

    let dir = args
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var("MANGA_DIR").ok())?;

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            n.ends_with(".jpg") || n.ends_with(".jpeg") || n.ends_with(".png") || n.ends_with(".webp")
        })
        .collect();
    // 文件名是 0.jpg / 1.jpg …，字典序排一遍就是页序
    files.sort();

    if files.is_empty() {
        None
    } else {
        Some(files.into_iter().map(LocalPage::File).collect())
    }
}

/// 命令行 `--server http://x:8001` 或环境变量 `MANGA_SERVER`。
/// 给了就跳过 UDP 发现直接连 —— 调试和固定部署都用得上。
fn env_base() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--server") {
        if let Some(v) = args.get(i + 1) {
            let b = api::normalize_base(v);
            if !b.is_empty() {
                return Some(b);
            }
        }
    }
    std::env::var("MANGA_SERVER")
        .ok()
        .map(|s| api::normalize_base(&s))
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- 服务器地址持久化

/// 存服务器地址的文件名。放在**二进制所在目录**下。
///
/// 用可执行文件自己的位置推导，不写死路径，这样两个环境都自动对上：
///   模拟器 `/mnt/us/emu/server.txt`
///   真机   `/mnt/us/extensions/kindle-manga/server.txt`
///
/// `/mnt/us` 是可写的 FAT32，所以真机上存得下去。
const SERVER_FILE: &str = "server.txt";

/// 全刷间隔存在这个文件里，内容就是一个十进制数字。
/// 和 `server.txt` 一样放程序目录，同样只在 FAT32 上落到 `/mnt/us`。
const REFRESH_FILE: &str = "refresh.txt";

/// 首次进阅读器的说明是否已经看过。文件存在 = 看过，不再弹。
/// 位置同 `server.txt`。规格 4.9 要求「点过知道了永不再现」。
const HINT_FILE: &str = "reader_hint_seen.txt";

/// 主页 / 设置·关于页显示的作者名。
///
/// ⚠️ 别改回占位符。设计规格里这一项标着「待填」，
/// 但用户已经给过正式署名了 —— 主页上挂一个「待填」会很显眼。
const APP_AUTHOR: &str = "Zhriav_Wu&DeepSeek";

fn config_path_named(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(name))
}

fn config_path() -> Option<PathBuf> {
    config_path_named(SERVER_FILE)
}

/// 收藏文件的位置。和 `server.txt` 同一套定位方式，交给 favorites 模块读写。
fn favorites_path() -> Option<PathBuf> {
    config_path_named(favorites::FILE)
}

/// 最近搜索文件的位置。同样落在程序目录。
fn recent_path() -> Option<PathBuf> {
    config_path_named(recent::FILE)
}

/// 阅读器那次性说明看过没有。读不出来一律当「没看过」——
/// 宁可多看一次说明，也别让第一次用的人什么都看不到。
fn hint_seen() -> bool {
    config_path_named(HINT_FILE)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// 记下「说明看过了」。写不进去也不影响本次（只是下次还会再弹一次）。
fn mark_hint_seen() {
    if let Some(p) = config_path_named(HINT_FILE) {
        if let Err(e) = std::fs::write(&p, b"1") {
            log::warn!("记录阅读器说明标记失败：{e}");
        }
    }
}

/// 读用户手填并保存下来的地址。
fn saved_base() -> Option<String> {
    let text = std::fs::read_to_string(config_path()?).ok()?;
    let b = api::normalize_base(&text);
    if b.is_empty() {
        None
    } else {
        Some(b)
    }
}

/// 保存地址。传空字符串表示清除（下次回到自动发现）。
fn save_base(base: &str) -> Result<(), String> {
    let path = config_path().ok_or("找不到程序所在目录")?;
    if base.is_empty() {
        // 删除失败不算错 —— 文件本来就不存在也很正常
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    std::fs::write(&path, base).map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 读用户存下来的全刷间隔。
///
/// 读不出来（文件不在、内容不是数字、超出上限）一律返回 None，
/// 由调用方回落到默认值 —— 这种情况下不报错，用默认值就是了。
fn saved_refresh_interval() -> Option<u32> {
    let text = std::fs::read_to_string(config_path_named(REFRESH_FILE)?).ok()?;
    text.trim().parse::<u32>().ok().filter(|v| *v <= FULL_REFRESH_MAX)
}

fn save_refresh_interval(interval: u32) -> Result<(), String> {
    let path = config_path_named(REFRESH_FILE).ok_or("找不到程序所在目录")?;
    std::fs::write(&path, interval.to_string())
        .map_err(|e| format!("写入 {} 失败：{e}", path.display()))
}

/// 按优先级列出候选地址，**但只列「来源存在」的，还没探活**。
///
/// 顺序：命令行 `--server` > 用户手填（`server.txt`）> `MANGA_SERVER` 环境变量。
fn candidate_bases(skip_saved: bool, skip_env: bool) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();

    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--server") {
        if let Some(v) = args.get(i + 1) {
            let b = api::normalize_base(v);
            if !b.is_empty() {
                out.push((b, "命令行 --server"));
            }
        }
    }

    if !skip_saved {
        if let Some(b) = saved_base() {
            out.push((b, "用户手填"));
        }
    }
    if !skip_env {
        if let Some(b) = env_base() {
            out.push((b, "MANGA_SERVER"));
        }
    }
    out
}

/// 找出一个能连上的服务端。**整个跑在后台线程上**（见 `Worker::boot`）。
///
/// 最多要试：若干个候选地址（各自有超时）+ 一次局域网广播（3 秒）。
/// 全落空的话十几秒是有的 —— 这段原来在 worker 上同步跑，等于点一下
/// 「重试」或者「重新寻找服务端」就把整个应用冻十几秒，期间的点击全排队。
///
/// 返回 `(找到的地址, 是否该跳过手填地址, 是否该跳过环境变量)`。
/// 后两个是「这次所有候选都没探通」的结论，要写回 worker，
/// 下次直接走广播、不再白探一遍（见 README 阶段 9.1）。
fn discover_server(
    known: Option<String>,
    mut skip_saved: bool,
    mut skip_env: bool,
    tx: &Sender<Cmd>,
    seq: u64,
) -> (Option<(String, api::Info)>, bool, bool) {
    // 进度要让用户看见：这十几秒里屏幕上得有点动静，
    // 否则和「点了没反应」长得一模一样。
    let status = |text: &str| {
        let _ = tx.send(Cmd::BootStatus {
            seq,
            text: text.to_string(),
        });
    };

    // 已经连上过的地址先探一下 —— 也许只是刚才抖了一下。
    if let Some(base) = known {
        if let Ok(info) = api::info(&base) {
            log::info!("沿用之前连上的 {base}");
            return (Some((base, info)), skip_saved, skip_env);
        }
        log::warn!("之前连上的 {base} 现在不通了，重新选一个");
    }

    // ---- 1. 候选地址逐个探活 ----
    let cands = candidate_bases(skip_saved, skip_env);
    if !cands.is_empty() {
        let names: Vec<&str> = cands.iter().map(|(_, s)| *s).collect();
        log::info!(
            "候选地址 {} 个（{}），逐个探活",
            cands.len(),
            names.join(" / ")
        );
        status(&format!("正在连接（试 {} 个候选地址）…", cands.len()));

        if let Some((base, info)) = pick_reachable(&cands) {
            return (Some((base, info)), skip_saved, skip_env);
        }

        // 全都探不通：记下来，下次重试时跳过它们直接走广播
        log::warn!("所有候选地址都探不通");
        skip_saved = true;
        skip_env = true;
    }

    // ---- 2. 广播发现 ----
    log::info!("改用局域网广播发现");
    status("正在局域网里找服务端…");

    if let Some(d) = api::discover(api::DISCOVER_TIMEOUT) {
        log::info!("广播发现到 {}", d.base);
        // 广播回来的地址也探一次活 —— 回包能到不代表 HTTP 一定通
        match api::info(&d.base) {
            Ok(info) => return (Some((d.base, info)), skip_saved, skip_env),
            Err(e) => log::warn!("广播发现的 {} 探活失败：{e}", d.base),
        }
    }

    (None, skip_saved, skip_env)
}

/// 依次探活候选地址，**返回第一个真的连得上的**。
///
/// ⚠️ 这里必须真的探活，不能只看「来源存在就用」。
///
/// 踩过这个坑：原先的写法是「`server.txt` 里有地址就直接用」，探活失败才在
/// 下一次重试时跳到下一个来源。结果 `server.txt` 里存着一个过期地址、
/// 而 `MANGA_SERVER` 是好的时候，第一次连接**必定**用那个坏地址失败 ——
/// 用户看到的是「明明配了地址却说连不上」，得手动点一次重试才好。
///
/// 现在改成先探活再选，多花一次往返（好地址约 20ms）换来「一次就对」。
fn pick_reachable(cands: &[(String, &'static str)]) -> Option<(String, api::Info)> {
    for (base, source) in cands {
        let t0 = std::time::Instant::now();
        match api::info(base) {
            Ok(info) => {
                log::info!("地址 {base}（来源：{source}）探活成功，耗时 {}ms", t0.elapsed().as_millis());
                return Some((base.clone(), info));
            }
            Err(e) => {
                log::warn!("地址 {base}（来源：{source}）探活失败：{e}");
            }
        }
    }
    None
}

// ---------------------------------------------------------------- 状态

/// 屏幕编号。**和 `ui/app.slint` 文件头的编号表一一对应**，改这里要同步改那边。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Screen {
    /// 0 历史记录（**首屏**，启动就进它）
    History = 0,
    /// 1 搜索（自带输入框 + 候选区 + 键盘；也兼作「填地址/填数字」的输入面）
    Search = 1,
    /// 2 主页（v5 变成 Logo + 作者 + 快速入门的关于页）
    Home = 2,
    /// 3 收藏
    Favorites = 3,
    /// 4 设置·第 1 页
    Settings = 4,
    /// 5 设置·关于（第 2 页）
    SettingsAbout = 5,
    /// 6 快速入门
    Guide = 6,
    /// 7 本子详情 / 章节目录
    Album = 7,
    /// 8 阅读器：整屏，没有顶栏底栏
    Reader = 8,
    /// 9 出错页 / 加载态
    Error = 9,
}

/// 搜索页输入框这次是拿来干什么的。
///
/// v5 把独立键盘屏删掉了，**搜索页是唯一的输入面** —— 填服务器地址、
/// 填全屏刷新间隔、跳页都复用它，只是确认键的去向不同。
/// 决定「打字要不要走拼音」的也是它：只有搜索才出候选。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum InputPurpose {
    /// 搜关键词（走拼音输入法，在搜索屏）
    Search,
    /// 手输服务器地址（字母数字混合，在搜索屏）
    Server,
    /// 全屏刷新间隔（纯数字，走覆盖式小键盘）
    Refresh,
    /// 阅读器里「跳转到页」（纯数字，走覆盖式小键盘）
    Page,
    /// 按本子 ID 打开（纯数字，走覆盖式小键盘）
    AlbumId,
}

/// 数字小键盘按「确定」之后该做什么。
///
/// 抽成**纯函数**是为了能用测试钉住边界 —— 页码夹取、空输入、非法输入
/// 这些「写错了不报错、只是跳错地方或静默失败」的情况，只有测试能拦。
#[derive(Debug, PartialEq, Eq)]
enum KeypadAction {
    /// 什么都不做，直接关掉覆盖层（空页码 / 空 ID）
    Close,
    /// 应用全屏刷新间隔
    FullRefresh(u32),
    /// 跳到第 N 页（**1 起**）
    GotoPage(usize),
    /// 按本子 ID 打开
    OpenAlbum(String),
    /// 非法输入：显示一行反馈、**不关**覆盖层
    Invalid(&'static str),
}

/// 决定小键盘「确定」的行为。`value` 是当前输入框里的数字串。
fn keypad_action(purpose: InputPurpose, value: &str, page_total: usize) -> KeypadAction {
    let v = value.trim();
    match purpose {
        InputPurpose::Refresh => {
            // 空 = 清除 / 回默认（默认是「每次全刷」）。
            // 这一项空着按确定**有明确语义**，不像页码/ID 那样直接关掉。
            if v.is_empty() {
                return KeypadAction::FullRefresh(FULL_REFRESH_DEFAULT);
            }
            match v.parse::<u32>() {
                Ok(n) => KeypadAction::FullRefresh(n.min(FULL_REFRESH_MAX)),
                Err(_) => {
                    KeypadAction::Invalid("请输入数字：0 = 关闭，或者每 N 次刷新全刷一次")
                }
            }
        }
        InputPurpose::Page => {
            // 空着按确定 = 不改，直接关掉，不报错
            if v.is_empty() {
                return KeypadAction::Close;
            }
            match v.parse::<usize>() {
                // 页码夹到 1..=总页数：填 0 会贴到第 1 页，填超了贴到最后一页。
                // 这样「多按了个 0」不会什么都不发生，也不会翻出边界。
                Ok(n) if page_total > 0 => KeypadAction::GotoPage(n.clamp(1, page_total)),
                Ok(_) => KeypadAction::Close, // 没有页（还没打开章节）→ 关掉
                Err(_) => KeypadAction::Invalid("请输入页码"),
            }
        }
        InputPurpose::AlbumId => {
            if v.is_empty() {
                return KeypadAction::Close;
            }
            // 本子 ID 是纯数字。小键盘只出数字，走到非数字说明有谁绕过了输入，
            // 仍然给反馈而不是静默失败。
            if v.bytes().all(|b| b.is_ascii_digit()) {
                KeypadAction::OpenAlbum(v.to_string())
            } else {
                KeypadAction::Invalid("本子 ID 只能是数字")
            }
        }
        // 这两个走搜索屏那套输入面，不属于小键盘
        InputPurpose::Search | InputPurpose::Server => KeypadAction::Close,
    }
}

/// 列表里的一格。
#[derive(Clone)]
struct RowData {
    label: String,
    sub: String,
    /// **标题下面那行小字**，空串 = 不画（这时标题照旧单行垂直居中）。
    ///
    /// 目前只有收藏页的单话用得上：上面是所属本子的标题，下面标「第 N 话」。
    /// 其余各屏一律空串，所以它们的外观一个像素都没变。
    sub_label: String,
    /// 绝对下标（搜索结果/历史是本子序号，目录是章节序号）
    idx: i32,
    cover: Option<PixelBuffer>,
    /// 右侧星标：0 = 不显示，1 = 空心（可收藏），2 = 实心（已收藏）。
    /// 收藏页全是 2；搜索结果 / 历史 / 目录用 1/2 表示「可点星收藏」。
    star: i32,
}

/// 星标取值。抽成函数是为了「搜到哪儿都算同一个值」，不会某处写 1 某处写 2。
fn album_star(favs: &favorites::Favorites, id: &str) -> i32 {
    if favs.contains_album(id) {
        2
    } else {
        1
    }
}

fn chapter_star(favs: &favorites::Favorites, id: &str) -> i32 {
    if favs.contains_chapter(id) {
        2
    } else {
        1
    }
}

/// 封面框的尺寸，按图的实际比例算。`None` 时给最大框当占位。
///
/// 抽出来是为了让「没封面」和「有封面」两条路都走同一个换算，
/// 不会出现只改了一边的情况。
fn cover_box(cover: Option<&PixelBuffer>, max_w: u32, max_h: u32) -> (u32, u32) {
    match cover {
        Some(b) => render::fit_box(b.width(), b.height(), max_w, max_h),
        None => (max_w, max_h),
    }
}

/// 某一页要切成几段（0/1 = 不用解密）。
///
/// 字段的优先级见 `descramble.rs` 顶部那张表：
///   1. 服务端逐页给 `seg` —— 直接用；
///   2. 服务端给 `filename` + 顶层 `scramble_id` —— 客户端算；
///   3. 只有 `scramble_id` —— 文件名按 5 位页号猜（实测源站都是这个命名）；
///   4. 什么都没有 —— 0，不解密（兼容旧版 Python 服务端）。
///
/// `aid` 是**章节 ID**（jmcomic 里 `JmImageDetail.aid` 就是 photo_id），
/// 不是本子 ID。
fn page_seg(photo: &api::Photo, page: usize) -> u32 {
    let Some(img) = photo.images.get(page) else {
        return 0;
    };
    if let Some(n) = img.seg {
        return n;
    }
    let (Ok(aid), Some(sid)) = (photo.id.parse::<u64>(), photo.scramble_id) else {
        return 0;
    };
    let name = if img.filename.is_empty() {
        format!("{:05}", page + 1)
    } else {
        img.filename.clone()
    };
    descramble::segmentation_num(sid, aid, &name)
}

/// 详情页上的一个标签。位置的 x/y/w 由 Rust 算好 ——
/// Slint 没有内置的换行流式布局，而标签的数量和长度都是变的。
#[derive(Clone)]
struct TagData {
    label: String,
    x: i32,
    y: i32,
    w: i32,
    idx: i32,
}

/// 推给 UI 的一整屏状态。
struct View {
    screen: Screen,
    /// 状态条文字。连上后带 IP，没连上是「服务器　未连接」
    conn_text: String,
    /// 底栏高亮第几项，-1 = 都不亮
    nav_active: i32,

    // ---- 列表通用 ----
    rows: Vec<RowData>,
    col_left: Vec<RowData>,
    col_right: Vec<RowData>,
    list_info: String,
    has_prev: bool,
    has_next: bool,
    list_sub: String,

    // ---- 搜索 / 输入 ----
    /// 输入框里的内容：**已确定的汉字 + 还没确定的拼音原文拼在一起**
    /// （见 `Worker::input_text`）。这就是按搜索时会搜到的东西。
    input_text: String,
    /// 键盘竖起没有。键盘是覆盖在结果区上的，不挤压它
    kb_shown: bool,
    /// 搜索结果的行高：把结果区那 1128px 均分给当前页的结果条数。
    /// 这样有几条铺几条，底下不留空白（和历史页 8 行均分同一个做法）。
    row_h: i32,
    /// 最近搜索的关键词（最新在前，最多 `recent::MAX` 条）。
    recent: Vec<String>,
    /// 现在该不该把最近搜索铺在结果区上。
    ///
    /// 条件：在搜索页、列表是空的、没在加载、且确实有历史词。
    /// 由 Rust 算好，UI 不用自己拼这几个状态 —— 少一个判据就会在
    /// 「搜索中」或「没有搜到」时错误地闪一下最近搜索。
    show_recent: bool,
    /// 键盘三行当前显示什么（字母页 / 符号页）
    kb_rows: [Vec<String>; 3],
    /// 键盘布局版本号，只有字母/符号切换时才 +1
    kb_seq: u64,
    /// 候选词，**横排一行 5 个**（v6，见 `ime::PAGE`）
    cand_row_a: Vec<String>,
    /// 选中候选在本页里的下标，-1 = 没选中
    cand_sel: i32,
    /// 候选格字号，由本页最长的那条候选算出来（见 `cand_font_size`）
    cand_fs: i32,
    /// 候选词栏的说明文字。**只有空状态用得上**（v6 取消了页码显示）——
    /// 到没到头看两端尖角的黑/灰就够了
    cand_note: String,
    /// 候选区是不是空状态（决定显示大段说明还是候选格）
    cand_empty: bool,
    /// 候选还能不能往前/往后翻。到头那一侧的「<」「>」显示成 #999 禁用态 ——
    /// 不然用户会一直点一个不会有反应的按钮。
    cand_has_prev: bool,
    cand_has_next: bool,
    /// 排序按钮的文字，如 ["最新", "最高评分", …]。空表示不显示这一行。
    sort_labels: Vec<String>,
    /// 当前选中的排序下标，-1 = 没有
    sort_active: i32,

    // ---- 设置 ----
    /// 「地址」行右边显示的当前地址
    set_addr: String,
    /// 「连接状态」行右边
    set_conn: String,
    /// 「全屏刷新」行右边，如「每 3 次刷新」
    refresh_label: String,
    /// 这台设备有没有电子墨水屏。桌面预览是 false，界面上要藏掉这一项。
    refresh_available: bool,
    /// 「下载缓存」行右边
    cache_label: String,
    version_label: String,
    device_label: String,
    server_label: String,
    pinyin_label: String,
    author_label: String,

    // ---- 收藏 ----
    fav_tab: i32,
    fav_right: String,

    // ---- 数字小键盘覆盖层 ----
    keypad_shown: bool,
    keypad_title: String,
    keypad_value: String,
    /// 已输数字的**字符个数**。Slint 字符串没有 .length，光标位置只能这么算。
    keypad_len: i32,
    keypad_note: String,

    // ---- 快速入门 ----
    guide_page: i32,

    // ---- 出错页 / 加载 ----
    err_title: String,
    err_desc: String,
    err_addr: String,
    err_state: String,
    err_last: String,
    loading: bool,
    loading_text: String,
    loaded_count: i32,
    total_count: i32,

    // ---- 详情页 ----
    album_title: String,
    /// 作者 + 车牌号（本子 ID）拼成的一行，如「真田一辉　·　ID 65539」。
    /// **在 Rust 里拼**：Slint 量不了文字宽度，两个 Text 想挨着排就得知道前一个多宽。
    album_byline: String,
    album_cover: Option<PixelBuffer>,
    /// 封面框的尺寸，按图的真实比例算好（见 render::fit_box）。
    /// 没封面时是最大框，当占位。
    album_cover_w: i32,
    album_cover_h: i32,
    album_tags: Vec<TagData>,

    // ---- 阅读器 ----
    page_image: Option<PixelBuffer>,
    reader_title: String,
    scroll_mode: bool,
    fit_label: String,
    reader_menu: bool,
    /// 长条漫不显示「适应」按钮
    reader_can_fit: bool,
    /// 菜单标题右「第 12 话 · 8 / 24」
    reader_pos: String,
    /// 「章节列表」行右「共 24 话」
    reader_chapters: String,
    /// 本话收藏了没有
    reader_starred: bool,
    /// 首次进阅读器的一次性说明
    reader_hint: bool,

    /// 内容版本号。只有真的变了才 +1，UI 侧据此跳过没必要的整屏重绘 ——
    /// 电子墨水刷一次要几百毫秒，能省则省。
    image_seq: u64,
    rows_seq: u64,
}

/// UI 发来的意图。
enum Cmd {
    Boot,
    Quit,
    Back,
    Retry,

    /// 底栏五项：0 历史 1 搜索 2 主页 3 收藏 4 设置
    NavPicked(i32),
    ListPrev,
    ListNext,
    /// 点第 idx 行（各屏含义见 `activate`）
    Activate(i32),
    /// 点第 idx 行右侧星标：收藏 / 取消收藏
    StarHit(i32),
    /// 点标签 → 按标签搜
    TagActivated(usize),
    /// 点搜索页「最近搜索」里的第 i 条 → 直接用它重搜
    RecentPicked(usize),
    /// 清空最近搜索
    RecentClear,
    /// 换排序方式（下标指 `sort_options`），换完重搜一次
    SetSort(usize),

    // 搜索 / 输入
    /// 字母键。**传字符本身**（不是下标），见 app.slint 的回调注释
    KeyChar(String),
    /// 符号 / 字母键盘切换
    KeySymbols,
    KeySpace,
    KeyBackspace,
    KeyConfirm,
    /// 点了候选栏里第 i 个（本页内下标）
    ImePick(usize),
    /// 候选翻页，-1 上一页 / +1 下一页
    ImePage(i32),
    /// 点输入框：把键盘叫回来
    KbWanted,
    /// 点「收起」：放下键盘
    KbHide,

    // 设置
    SetEditServer,
    SetEditRefresh,
    /// 重新探一次服务端
    SetRecheck,
    SetClearCache,
    /// 设置两页之间翻页
    PageLeft,
    PageRight,

    /// 主页「按本子 ID 打开」→ 弹数字小键盘
    OpenById,

    // 数字小键盘覆盖层（全屏刷新 / 跳页 / 本子 ID 三处共用）
    /// 按下一个数字键，**传字符本身**
    KeypadDigit(String),
    KeypadBackspace,
    KeypadClear,
    /// 「取消」：直接关掉覆盖层，**不改任何设置、不跳屏**
    KeypadCancel,
    /// 「确定」：按用途分派
    KeypadOk,

    /// **搜索请求回来了**（后台线程发回来的，不是 UI 点的）。
    ///
    /// 带着发起时的 `(关键词, 排序, 页码)` 当**身份** —— 回来时对不上就说明
    /// 用户已经改了词 / 换了排序 / 翻到别的页了，这一份直接丢掉。
    /// 异步化必须配这个校验，否则「搜 A 的结果」会贴到「搜 B 的界面」上。
    SearchDone {
        keyword: String,
        order: String,
        page: u32,
        found: Result<api::SearchResult, api::ApiError>,
    },

    /// **连接流程的一句进度**（后台线程发回来的）。
    ///
    /// 「正在连接（试 3 个候选地址）…」这类。发现过程最长十几秒，
    /// 中间得让用户看见点动静，否则和「点了没反应」没区别。
    BootStatus {
        seq: u64,
        text: String,
    },

    /// **连接流程跑完了**（后台线程发回来的）。
    BootDone {
        seq: u64,
        /// `None` = 一个地址都没连上，该走出错页
        found: Option<(String, api::Info)>,
        /// 「这次所有候选都没探通」的结论，写回 worker 供下次跳过
        skip_saved: bool,
        skip_env: bool,
    },

    /// **历史记录请求回来了**（后台线程发回来的）。
    HistoryDone(Result<Vec<api::HistoryItem>, api::ApiError>),

    /// **本子详情请求回来了**（后台线程发回来的）。
    AlbumDone {
        album_id: String,
        /// 发起请求时用户在哪一屏。详情页的「返回」要回到那儿。
        ///
        /// ⚠️ 必须在发请求时就记下来、跟着结果一起带回来，**不能等回来时
        ///    再读 `self.screen`** —— 那时用户可能已经翻到别处了，
        ///    记下的「来路」就成了错的，返回会去错的地方。
        from: Screen,
        found: Result<api::Album, api::ApiError>,
        /// 试了几次才成功（>1 说明网络抖过）
        tries: u32,
        /// 这次请求花了多少毫秒（失败提示里要用）
        ms: u64,
    },

    /// **「打开本子并跳进某一话」请求回来了**（收藏的单话入口用的）。
    AlbumChapterDone {
        album_id: String,
        photo_id: String,
        from: Screen,
        found: Result<api::Album, api::ApiError>,
    },

    /// **章节详情请求回来了**（后台线程发回来的）。
    PhotoDone {
        photo_id: String,
        album_title: String,
        /// 章节名，失败提示里要用
        label: String,
        found: Result<api::Photo, api::ApiError>,
    },

    /// **一页漫画取回并解好了**（后台线程发回来的）。
    ///
    /// 这是最重的一条：几百 KB 传输 + 解码 + 缩放，原来全在 worker 上同步做，
    /// 就是「翻一页要等好几秒、期间点什么都没反应」的来源。
    PageReady {
        /// 身份 = (章节, 页码, 适屏方式)，见 `Worker::page_req`
        key: String,
        page: usize,
        fit: render::FitMode,
        result: Result<DecodedPage, String>,
    },

    /// **详情页那张封面取回来了**（后台线程发回来的）。
    AlbumCoverDone {
        url: String,
        thumb: Option<PixelBuffer>,
    },

    /// **预取的下一页字节到了**（后台线程发回来的）。
    ///
    /// 只往缓存里塞，不碰界面 —— 预取的意义就是「等你真翻到那一页时不用等」，
    /// 它自己不该引起任何重绘。
    Prefetched {
        key: (String, usize),
        bytes: Vec<u8>,
    },

    /// **封面取回来了**（另一个线程取完发回来的，不是 UI 点的）。
    ///
    /// 为什么要有这条：取封面要 HTTP + 解码，在真机上单张就要几百毫秒，
    /// 六张就是好几秒。原来这段是在 worker 线程里同步跑的，那几秒里
    /// worker 完全被占住，用户点的东西全堵在通道里 —— 等封面取完，
    /// 那些点击**一次性全部执行**，界面连着跳好几下（用户形容成
    /// 「跟末影珍珠一样」）。现在取图挪到后台线程，worker 只负责
    /// 「收到一张就贴上去」，于是点击任何时候都能立刻处理。
    ///
    /// `epoch` 用来丢弃**过期**的结果：换屏 / 重铺列表之后，
    /// 上一屏还没取完的封面回来时不能再往这一屏的行上贴。
    CoverReady {
        epoch: u64,
        idx: usize,
        url: String,
        thumb: Option<PixelBuffer>,
    },

    // 收藏
    FavTabPicked(i32),

    // 快速入门
    GuidePrev,
    GuideNext,

    // 阅读器
    ReaderMenuToggle,
    ReaderFitToggle,
    ReaderRefresh,
    ReaderPrev,
    ReaderNext,
    ReaderGotoPage,
    ReaderOpenChapters,
    ReaderStarToggle,
    ReaderExit,
    ReaderHintDismiss,
}

// ---------------------------------------------------------------- 取图（后台线程用）

/// 后台线程解好的一页（见 [`Cmd::PageReady`]）。
struct DecodedPage {
    /// 原始字节。worker 收下后放进 `raw_cache`，翻回来时不用再下载。
    raw: Vec<u8>,
    /// 长条漫留住整条（滚动要用它反复裁）；普通页是 None
    strip: Option<PixelBuffer>,
    strip_step: usize,
    strip_y: usize,
    /// 真正交给 UI 的那张整屏画布
    display: PixelBuffer,
    /// 是不是长条漫
    long: bool,
}

/// 把已经解出来的图裁成「能直接上屏的画布」。
///
/// 纯计算，不碰网络 —— 本地模式和网络模式共用这一段，区别只在字节哪来的。
fn finish_decode(
    buf: PixelBuffer,
    raw: Vec<u8>,
    fit: render::FitMode,
    seg: u32,
    orig_h: u32,
) -> DecodedPage {
    // ---- 禁漫横条重排解密 ----
    //
    // ⚠️ 必须在**缩放/切屏之前**做：分段边界是按原图高度算的。
    //    `orig_h` 是解码前的真实高度，解码阶段被降采样过时靠它做比例映射。
    //
    // seg <= 1 = 不用解密：旧服务端发的是已解密的图，这条路径保证兼容。
    let buf = if seg > 1 {
        descramble::descramble_scaled(&buf, seg, orig_h)
    } else {
        buf
    };

    let long = render::is_long_strip(&buf);
    // 缩放是这里最贵的一步（Kindle 的 CPU 比电脑慢不少）。慢的时候记一行，
    // 正常情况下不刷屏 —— 有了这个数字才知道要不要再优化。
    let t0 = std::time::Instant::now();
    let (src_w, src_h) = (buf.width(), buf.height()); // buf 稍后被 move 进 DecodedPage
    let (strip, strip_step, strip_y, display) = if long {
        // 长条漫：留住整条（后面滚动要用它反复裁），显示第一屏。
        // 它只按宽度适配 + 纵向切片，不参与「竖边/横边」那套切换 ——
        // 一条两万像素高的图按高度缩进去就成一根线了。
        let step = render::strip_page_step(&buf);
        let (canvas, y, _) = render::slice_to_canvas(&buf, 0);
        (Some(buf), step, y, canvas)
    } else {
        // 普通页：**在 Rust 里裁成整屏画布**再交给 UI。
        //
        // 为什么不让 Slint 的 image-fit 去缩：UI 那边用的是 1:1 的 fill
        //（图已经是整屏尺寸了），缩放和居中都在这儿一次算完。
        // 少一层计算，电子墨水上就少一点闪烁。
        (None, 0, 0, render::fit_to_canvas(&buf, fit))
    };
    let scale_ms = t0.elapsed().as_millis();
    if scale_ms > 150 {
        log::warn!(
            "缩放偏慢：{}×{} → 画布 {}×{} 用了 {}ms{}",
            src_w,
            src_h,
            display.width(),
            display.height(),
            scale_ms,
            if long { "（长条漫）" } else { "" }
        );
    }
    DecodedPage {
        raw,
        strip,
        strip_step,
        strip_y,
        display,
        long,
    }
}

/// 本地模式用：字节 → 解码 → 裁屏。
///
/// 本地图（`--demo` / `--dir`）不是从禁漫源站来的，**没有加密**，所以 seg 恒 0。
fn decode_page(bytes: Vec<u8>, fit: render::FitMode) -> Result<DecodedPage, String> {
    let d = render::decode_meta(&bytes).map_err(|e| format!("解码失败：{e}"))?;
    Ok(finish_decode(d.buf, bytes, fit, 0, d.orig_h))
}

/// 网络模式用：**取一页原图 → 解码 → 裁屏，整个跑在后台线程上。**
///
/// 真机上这几步合起来要几百毫秒到几秒（几百 KB 的传输 + 解码 + 缩放），
/// 原来全在 worker 上同步做 —— 那就是「翻一页要等好几秒、期间点什么都没反应」
/// 的来源。
fn fetch_and_decode_page(
    base: &str,
    url: &str,
    jpeg_url: &str,
    fit: render::FitMode,
    seg: u32,
) -> Result<DecodedPage, String> {
    let bytes = match api::retry(3, || api::fetch_bytes(base, url)).map(|(v, _)| v) {
        Ok(Some(b)) => b,
        // 404 = 越界，是正常的翻页边界，不是故障
        Ok(None) | Err(api::ApiError::End) => return Err("这一页服务端取不到".into()),
        Err(e) => return Err(format!("取图失败：{e}")),
    };

    // 原格式解不动时的兜底：让服务端转成 JPEG 再取一次。
    //
    // 服务端默认发原格式（多半是 webp，体积最省），但动图 webp、某些少见的
    // png 客户端解码器不一定吃得下。与其在客户端猜格式，不如让服务端转 ——
    // 它那边有 PIL，转一次还会缓存。
    //
    // ⚠️ 走兜底时**照样要解密**：服务端转码只是换了容器，横条顺序没动，
    //    seg 原样传进去。
    let first_err = match render::decode_meta(&bytes) {
        Ok(d) => return Ok(finish_decode(d.buf, bytes, fit, seg, d.orig_h)),
        Err(e) => e,
    };
    if !jpeg_url.is_empty() {
        if let Some(jb) = api::fetch_bytes(base, jpeg_url).ok().flatten() {
            if let Ok(d) = render::decode_meta(&jb) {
                return Ok(finish_decode(d.buf, jb, fit, seg, d.orig_h));
            }
        }
    }
    Err(format!("解码失败：{first_err}"))
}

// ---------------------------------------------------------------- Worker

/// 独占全部状态的后台工作者。
struct Worker {
    weak: slint::Weak<AppWindow>,

    /// 服务端地址，形如 `http://192.168.1.20:8001`。None = 还没连上。
    base: Option<String>,
    /// 本地单章模式（模拟器自检用）。Some 时完全不碰网络。
    local: Option<Vec<LocalPage>>,

    screen: Screen,
    /// 「返回」要回到哪一屏（**输入页、设置、收藏**用它）
    nav_return: Screen,
    /// 本子目录（详情页）的「返回」回到哪一屏。
    ///
    /// ⚠️ 为什么不能用 `nav_return`：那个变量是「最后一个打开输入页/设置的地方」，
    /// 任何一次开输入页都会把它覆盖掉。而阅读器菜单里的「刷新频率」也会开输入页 ——
    /// 于是从阅读器进目录后，目录的「返回」就会弹回阅读器（死循环，只能退出程序）。
    /// 分开一个变量，两条路径就互不干扰了。
    album_return: Screen,

    status: String,
    list_info: String,
    /// 列表为空时居中显示的一句话（历史副标题 / 搜索引导 / 输入页提示）
    list_sub: String,
    loading: bool,
    loading_text: String,

    // 输入页（v5 合并进搜索屏）
    input: String,
    input_purpose: InputPurpose,
    /// 已经上屏的候选段：(汉字词, 产生它的拼音原文)。
    ///
    /// 记 raw 是为了让**删除键能把整段候选一次撤掉、退回字母原文**
    /// （规格 4.2 第 5 条），而不是逐个删汉字。
    committed: Vec<(String, String)>,
    /// 键盘竖着没有。竖着结果区 490px，放下 1128px
    kb_shown: bool,
    /// 键盘现在是符号页还是字母页
    kb_symbols: bool,
    /// 键盘布局版本号（字母/符号切换时 +1）
    kb_seq: u64,
    /// 拼音缓冲与候选（中文输入法，见 src/ime.rs）
    compose: ime::Compose,

    // 收藏（本地版，见 src/favorites.rs）
    favs: favorites::Favorites,
    /// 收藏页当前分栏：0 = 作品，1 = 单话
    fav_tab: i32,
    fav_page: usize,
    /// 收藏页当前显示的那一栏（下标对应 rows 的 idx）
    fav_items: Vec<favorites::Fav>,

    // 数字小键盘覆盖层（全屏刷新 / 跳页 / 本子 ID）
    //
    // ⚠️ 它**不改变 `screen`** —— 覆盖式的（规则 ⑨），开和关底下那一屏
    // 一个像素都不动。所以这里**绝不能碰 `nav_return`**：确定/取消后用户
    // 就该留在原来那一屏（设置页 / 阅读器 / 主页）。
    keypad_shown: bool,
    keypad_title: String,
    keypad_value: String,
    keypad_note: String,
    /// 这一下在输什么，决定「确定」的去向
    keypad_purpose: InputPurpose,

    // 快速入门
    guide_page: i32,

    // 出错页
    err_title: String,
    err_desc: String,
    err_addr: String,
    err_state: String,
    err_last: String,

    // 关于页
    /// 平台名（「桌面预览」/「Kindle E Ink」），关于页的「设备」行用它
    platform: String,

    // 全屏刷新（清残影）
    /// 后端策略句柄。桌面预览是空壳，见 `RefreshHandle`。
    refresh: RefreshHandle,
    /// 当前设置：每多少次刷新做一次全屏全刷。0 = 关闭。
    full_refresh: u32,
    /// 策略当前是不是「开着的」（= 在阅读器里）。
    ///
    /// `None` = 还没同步过。记这个是为了**只在进出阅读器时改一次**策略 ——
    /// 每帧都改会把它的计数器清零，永远攒不到触发条件。
    refresh_active: Option<bool>,

    // 搜索
    keyword: String,
    /// 最近搜索关键词（本地文件）。搜索页空态时铺在结果区上。
    recent: recent::Recent,
    order: String,
    /// 服务端支持的排序方式 (值, 显示名)。空 = 旧版服务端，不显示排序按钮。
    sort_options: Vec<(String, String)>,
    search_page: u32,
    search_page_count: u32,
    search_ids: Vec<String>,
    /// 还在「刚打开搜索页、一次都还没搜过」的状态。
    ///
    /// 只有这时候才铺「最近搜索」。搜完之后哪怕结果为空，
    /// 也应该显示「没有搜到…」而不是又冒出最近搜索 —— 那会让人以为搜索没执行。
    search_fresh: bool,

    // 当前屏的列表
    rows: Vec<RowData>,
    col_left: Vec<RowData>,
    col_right: Vec<RowData>,
    has_prev: bool,
    has_next: bool,

    // 历史
    history: Vec<api::HistoryItem>,
    history_page: usize,

    // 本子与章节
    album: Option<api::Album>,
    album_page: usize,
    photo: Option<api::Photo>,
    album_title: String,
    /// 详情页顶部的封面
    album_cover: Option<PixelBuffer>,
    /// 详情页的标签（位置已算好）
    album_tags: Vec<TagData>,
    /// 标签文字，点第 i 个就拿它去搜索
    tag_texts: Vec<String>,
    page: usize,

    // 阅读器
    /// 当前适屏方式（竖边 / 横边）
    fit_mode: render::FitMode,
    /// 菜单是否展开
    reader_menu: bool,
    /// 本话收藏了没有（菜单「收藏本话」两态）
    reader_starred: bool,
    /// 首次进阅读器的一次性说明还在不在
    reader_hint: bool,

    // 长条漫
    strip: Option<PixelBuffer>,
    strip_y: usize,
    strip_step: usize,

    // 当前显示的画面
    display: Option<PixelBuffer>,
    scroll_mode: bool,

    /// 封面缩略图缓存，键是封面 URL。None 表示取过但失败了，不必重试。
    covers: HashMap<String, Option<PixelBuffer>>,
    /// 原图字节缓存，键是 (章节, 页)。翻页时先看这里。
    raw_cache: HashMap<(String, usize), Vec<u8>>,

    /// 把命令送回自己的那条管道（封面线程取好图之后从这儿送回来）。
    ///
    /// worker 和 UI 共用同一个 channel —— 对 worker 来说它既是入口也是出口，
    /// 所以后台线程取完图只要往这儿 `send` 一条 [`Cmd::CoverReady`]，
    /// worker 就会像处理一次点击那样把它处理掉。不需要额外的同步原语。
    tx: Sender<Cmd>,
    /// 当前这份列表的代号。**每次重铺列表 +1**。
    ///
    /// 取封面是异步的，结果回来时可能已经换屏了；靠这个代号把
    /// 过期结果丢掉，避免「上一屏的封面贴到这一屏的行上」。
    list_epoch: u64,
    /// 最近一次发出的**搜索**请求的身份：`(关键词, 排序, 页码, 发起时在哪一屏)`。
    /// 只有和它完全一致、**且用户还没走开**的结果才允许上屏。
    /// `None` = 当前没有在飞的搜索。
    ///
    /// 为什么连「发起时在哪一屏」也要比：搜索请求发出去之后，用户随时可以
    /// 点底栏走人。不查这个的话，结果晚回来时会把用户**硬拽回搜索页**，
    /// 而且会把 `rows`（搜索/历史共用）覆盖掉 —— 正是 README 第 32 条
    /// 那类「多个屏共用一个字段」的坑，只是换成了异步版本。
    ///
    /// 比的是「还是不是原来那一屏」而不是「是不是搜索页」，因为**标签搜索**
    /// 是从详情页发起、靠结果回来才切到搜索页的（见 `Cmd::TagActivated`）。
    search_req: Option<(String, String, u32, Screen)>,
    /// 连接流程的轮次。每 `boot()` 一次 +1，只有最新一轮的结果能上屏。
    boot_seq: u64,
    /// 最近一次发出的**本子详情**请求的 `(id, 发起时在哪一屏)`。
    /// 对不上、或者用户已经走开了，结果就丢弃。
    ///
    /// 带屏是为了防「点进本子 → 趁加载时点底栏走人 → 结果回来把人拽回详情页」。
    album_req: Option<(String, Screen)>,
    /// 最近一次发出的**章节详情**请求的 id。回来时对不上就丢弃。
    photo_req: Option<String>,
    /// 最近一次发出的**整页图**请求的身份：`(章节, 页码, 适屏方式)`。
    ///
    /// 三个都要比：换了章节、翻了页、切了适屏方式，之前那张图就都不该再上屏。
    /// 少了任何一个，都会出现「翻到第 3 页却显示第 2 页」这种错位。
    page_req: Option<(String, usize, render::FitMode)>,

    image_seq: u64,
    rows_seq: u64,
    /// 上一次写进日志的界面，同一屏不重复刷
    logged: Option<(Screen, String)>,

    /// 跳过用户手填的地址（server.txt）。
    /// 地址连不上时置位，让下一次「重新寻找服务端」往下走一层。
    skip_saved: bool,
    /// 跳过 MANGA_SERVER 环境变量。同上。
    skip_env: bool,
}

impl Worker {
    fn new(
        weak: slint::Weak<AppWindow>,
        local: Option<Vec<LocalPage>>,
        refresh: RefreshHandle,
        full_refresh: u32,
        platform: String,
        tx: Sender<Cmd>,
    ) -> Self {
        Self {
            weak,
            base: None,
            local,
            // 首屏是历史记录（规格定的），不是主页
            screen: Screen::History,
            nav_return: Screen::History,
            album_return: Screen::History,
            status: "启动中…".into(),
            list_info: String::new(),
            list_sub: String::new(),
            loading: false,
            loading_text: "载入中…".into(),
            input: String::new(),
            input_purpose: InputPurpose::Search,
            committed: Vec::new(),
            kb_shown: true,
            kb_symbols: false,
            kb_seq: 0,
            compose: ime::Compose::new(),
            favs: favorites::Favorites::local(favorites_path()),
            fav_tab: 0,
            fav_page: 0,
            fav_items: Vec::new(),
            keypad_shown: false,
            keypad_title: String::new(),
            keypad_value: String::new(),
            keypad_note: String::new(),
            keypad_purpose: InputPurpose::Refresh,
            guide_page: 0,
            err_title: String::new(),
            err_desc: String::new(),
            err_addr: String::new(),
            err_state: String::new(),
            err_last: String::new(),
            platform,
            refresh,
            full_refresh,
            refresh_active: None,
            keyword: String::new(),
            recent: recent::Recent::local(recent_path()),
            // 服务端的 DEFAULT_SORT，连上后会用 /api/info 的值覆盖
            order: "mr".into(),
            sort_options: Vec::new(),
            search_page: 1,
            search_page_count: 1,
            search_ids: Vec::new(),
            search_fresh: true,
            rows: Vec::new(),
            col_left: Vec::new(),
            col_right: Vec::new(),
            has_prev: false,
            has_next: false,
            history: Vec::new(),
            history_page: 0,
            album: None,
            album_page: 0,
            photo: None,
            album_title: String::new(),
            album_cover: None,
            album_tags: Vec::new(),
            tag_texts: Vec::new(),
            page: 0,
            fit_mode: render::FitMode::default(),
            reader_menu: false,
            reader_starred: false,
            reader_hint: false,
            strip: None,
            strip_y: 0,
            strip_step: 0,
            display: None,
            scroll_mode: false,
            covers: HashMap::new(),
            raw_cache: HashMap::new(),
            tx,
            list_epoch: 0,
            search_req: None,
            boot_seq: 0,
            album_req: None,
            photo_req: None,
            page_req: None,
            image_seq: 0,
            rows_seq: 0,
            logged: None,
            skip_saved: false,
            skip_env: false,
        }
    }

    // ------------------------------------------------------------ 推送给 UI

    fn view(&self) -> View {
        let (cover_w, cover_h) = cover_box(
            self.album_cover.as_ref(),
            render::COVER_W,
            render::COVER_H,
        );
        let (cand_row, cand_sel, cand_fs, cand_note, cand_empty, cand_has_prev, cand_has_next) =
            self.cand_panel();
        View {
            screen: self.screen,
            conn_text: self.conn_text(),
            nav_active: nav_active(self.screen),
            rows: self.rows.clone(),
            col_left: self.col_left.clone(),
            col_right: self.col_right.clone(),
            list_info: self.list_info.clone(),
            has_prev: self.has_prev,
            has_next: self.has_next,
            list_sub: self.list_sub.clone(),
            input_text: self.input_text(),
            recent: self.recent.items().to_vec(),
            show_recent: self.screen == Screen::Search
                && self.search_fresh
                && self.rows.is_empty()
                && !self.loading
                && !self.recent.items().is_empty(),
            kb_shown: self.kb_shown,
            row_h: search_row_h(self.rows.len()),
            kb_rows: self.kb_rows(),
            kb_seq: self.kb_seq,
            cand_row_a: cand_row,
            cand_sel,
            cand_fs,
            cand_note,
            cand_empty,
            cand_has_prev,
            cand_has_next,
            sort_labels: self.sort_options.iter().map(|(_, l)| l.clone()).collect(),
            sort_active: self
                .sort_options
                .iter()
                .position(|(v, _)| *v == self.order)
                .map(|i| i as i32)
                .unwrap_or(-1),
            set_addr: self.set_addr(),
            set_conn: self.set_conn(),
            refresh_label: self.refresh_label(),
            refresh_available: self.refresh.available(),
            cache_label: format!(
                "已缓存 {} 封面 / {} 页原图",
                self.covers.len(),
                self.raw_cache.len()
            ),
            version_label: env!("CARGO_PKG_VERSION").to_string(),
            device_label: self.platform.clone(),
            server_label: self
                .base
                .as_deref()
                .map(strip_scheme)
                .unwrap_or("未连接")
                .to_string(),
            pinyin_label: format!("{} 条", ime::len()),
            author_label: APP_AUTHOR.to_string(),
            fav_tab: self.fav_tab,
            fav_right: self.fav_right(),
            keypad_shown: self.keypad_shown,
            keypad_title: self.keypad_title.clone(),
            keypad_value: self.keypad_value.clone(),
            keypad_len: self.keypad_value.chars().count() as i32,
            keypad_note: self.keypad_note.clone(),
            guide_page: self.guide_page,
            err_title: self.err_title.clone(),
            err_desc: self.err_desc.clone(),
            err_addr: self.err_addr.clone(),
            err_state: self.err_state.clone(),
            err_last: self.err_last.clone(),
            loading: self.loading,
            loading_text: self.loading_text.clone(),
            // 阅读器加载一页时给个「读到第几页 / 共几页」的进度方块；
            // 其余加载（搜索、取历史、开本子）没有可量化的进度，方块保持空心。
            loaded_count: self.loading_filled(),
            total_count: 6,
            album_title: self.album_title.clone(),
            album_byline: self.album_byline(),
            album_cover: self.album_cover.clone(),
            album_cover_w: cover_w as i32,
            album_cover_h: cover_h as i32,
            album_tags: self.album_tags.clone(),
            page_image: self.display.clone(),
            reader_title: self.reader_title(),
            scroll_mode: self.scroll_mode,
            fit_label: self.fit_mode.label().to_string(),
            reader_menu: self.reader_menu,
            reader_can_fit: !self.scroll_mode,
            reader_pos: self.reader_pos(),
            reader_chapters: self.reader_chapters(),
            reader_starred: self.reader_starred,
            reader_hint: self.reader_hint,
            image_seq: self.image_seq,
            rows_seq: self.rows_seq,
        }
    }

    /// 状态条文字。连上后带上地址（规格：状态条左「服务器　状态　IP:端口」）。
    fn conn_text(&self) -> String {
        match self.base.as_deref() {
            Some(b) => format!("服务器　已连接　{}", strip_scheme(b)),
            None => "服务器　未连接".to_string(),
        }
    }

    /// 设置页「地址」行。去掉 `http://`，规格里显示的是 `IP:端口`。
    fn set_addr(&self) -> String {
        self.base.as_deref().map(strip_scheme).unwrap_or("未设置").to_string()
    }

    /// 设置页「连接状态」行。
    fn set_conn(&self) -> String {
        if self.base.is_some() {
            "已连接 · 点此重测".to_string()
        } else {
            "未连接 · 点此重测".to_string()
        }
    }

    /// 输入框里最终要显示 / 要搜的东西 = **已上屏的汉字 + 还没确定的拼音原文**。
    ///
    /// ⚠️ 两者**必须拼在一起**。只显示已上屏部分的话，用户打 `yuan` 时输入框
    /// 是空的，看着像键盘坏了；拼上之后就和他敲进去的完全一致，
    /// 也就是按搜索时**真正会搜到的东西**。
    ///
    /// （右侧原来还有一行「将搜索：xxx」把这件事再说一遍，按用户要求去掉了 ——
    ///   输入框本身就是那个内容，重复显示没有新信息，还白占 340px 宽度。）
    fn input_text(&self) -> String {
        let mut s = self.input.clone();
        s.push_str(self.compose.buf());
        s
    }

    /// 键盘三行当前该显示什么。字母页 / 符号页。
    fn kb_rows(&self) -> [Vec<String>; 3] {
        let src = if self.kb_symbols { &KB_SYMBOLS } else { &KB_LETTERS };
        std::array::from_fn(|i| src[i].iter().map(|s| (*s).to_string()).collect())
    }

    /// 收藏页顶栏右边「共 17 部 / 共 8 话」。
    fn fav_right(&self) -> String {
        let (n, unit) = if self.fav_tab == 1 {
            (self.favs.chapters().len(), "话")
        } else {
            (self.favs.albums().len(), "部")
        };
        format!("共 {n} {unit}")
    }

    /// 候选词栏要显示的东西。
    ///
    /// 返回：候选（一行 5 个）、选中下标、字号、说明、是不是空状态、两端能不能翻。
    ///
    /// 一行 5 个是跟 `ui/app.slint` 绑定的（`ime::PAGE` = 5），
    /// 那边是 `108 + 204 × 5 + 108 = 1236`。改一处必须改另一处。
    ///
    /// 非搜索用途（填地址 / 填数字）没有候选，直接走空状态 —— 但**高度不变**，
    /// 说明文字就放 `cand_note`（规则 ⑩）。
    fn cand_panel(&self) -> (Vec<String>, i32, i32, String, bool, bool, bool) {
        if self.input_purpose != InputPurpose::Search || !self.compose.composing() {
            let note = match self.input_purpose {
                InputPurpose::Server => "输入服务器地址，如 192.168.1.91；点「符号」切数字和点号",
                // 数字三兄弟现在都走覆盖层了，这里只是兜底，正常到不了
                InputPurpose::Refresh => "输入数字：0 = 关闭，或每 N 次刷新全刷一次",
                InputPurpose::Page => "输入页码后按「搜索」跳过去",
                InputPurpose::AlbumId => "输入本子 ID（纯数字）后按「搜索」打开",
                InputPurpose::Search => "敲字母出候选；点候选变汉字，不点就直接搜字母原文",
            };
            return (Vec::new(), -1, 44, note.to_string(), true, false, false);
        }

        if self.compose.cands().is_empty() {
            // 表里找不到这串字母 —— 但这**不是死路**：按「搜索」会把它原样当英文搜。
            // 不自动清空缓冲：用户可能只是打多了一个字母，删掉就好。
            let note = format!("无匹配拼音「{}」 · 按「搜索」搜字母原文", self.compose.buf());
            return (Vec::new(), -1, 44, note, true, false, false);
        }

        let row: Vec<String> = self.compose.page_items().to_vec();
        // 字号跟着**本页最长的那条**走，整行统一 —— 单独给某一格变小的话，
        // 一行里字大小不一，看着像排版坏了。
        let fs = cand_font_size(row.iter().map(|s| s.chars().count()).max().unwrap_or(1));

        // 到头的那一侧要让尖角变灰（规格 5：全稿唯一的禁用表达）——
        // 否则用户会一直点一个不会有反应的按钮。
        // 翻页本身是夹住的（Compose::turn_page），这里只是把它反映到界面上。
        let has_prev = self.compose.page() > 0;
        let has_next = self.compose.page() + 1 < self.compose.page_count();

        (row, self.compose.sel_in_page() as i32, fs, String::new(), false, has_prev, has_next)
    }

    /// 把**选中的候选**落进输入框 —— 「上屏」。
    ///
    /// 只有「点候选栏」和「空格」走这里：那两下是用户明确挑了中文。
    /// 没有候选时什么都不做，缓冲原样留着，用户退格改字就是了。
    ///
    /// 上屏时把「汉字词 + 它的拼音原文」记进 `committed`，删除键才能整段撤回。
    fn commit_compose(&mut self) {
        let raw = self.compose.buf().to_string();
        if let Some(text) = self.compose.take() {
            self.input.push_str(&text);
            self.committed.push((text.clone(), raw));
            log::info!("候选上屏「{text}」，输入框现在是「{}」", self.input_text());
        }
    }

    /// 把还没挑候选的拼音缓冲**原样**落进输入框（当英文字母用）。
    ///
    /// 和 `commit_compose()` 的区别只有一条：**要不要变中文**。
    /// 点了候选 / 按了空格 = 用户明确要中文，走 `commit_compose()`；
    /// 其余情况（按确定、点数字符号）都走这里 —— 那串字母原样落下，
    /// 因为用户想搜英文时，打的本来就是英文。
    ///
    /// ⚠️ **别把这里并回 `commit_compose()`。** `take()` 不管有没有选中过都
    /// 返回 `cands[0]`，于是 `one` 会静默变成「哦呢」，而 `naruto`
    /// （表里没这个音）返回 `None`、输入框还是空的 ——
    /// 拼音模式下就彻底搜不了英文了。这两条路必须分开。
    fn raw_commit_text(&mut self) {
        let text = self.compose.take_raw();
        if !text.is_empty() {
            self.input.push_str(&text);
            log::info!("拼音按原样落定「{text}」，输入框现在是「{}」", self.input_text());
        }
    }

    /// 敲了一个键。字母和数字/符号走的路不一样。
    fn type_key(&mut self, key: &str) {
        let lower = key.to_ascii_lowercase();
        let is_letter = lower.len() == 1 && lower.as_bytes()[0].is_ascii_lowercase();

        // 只有搜索走拼音。填地址 / 填数字时打字母就是字母 ——
        // v5 没有「中/英」模式键了，这个判断改由「这次输入是干什么的」决定。
        if self.input_purpose == InputPurpose::Search && is_letter {
            self.compose.push(lower.as_bytes()[0] as char);
            return;
        }

        // 数字和符号（以及非搜索用途）：先把没落定的拼音**原样**落下来，再插进去。
        // 不落定的话，用户打「beijing1」会连「1」一起当成拼音字母丢掉；
        // 而按原样落（不是落选中的候选）才对 —— 用户没点候选就是没要中文，
        // 所以「beijing」+「1」得到的是「beijing1」，不是「北京1」。
        if !is_letter {
            self.raw_commit_text();
        }
        self.input.push_str(&lower);
    }

    /// 删除键。规格 4.2 第 5 条：
    /// 先退还没确定的拼音字母；拼音退光了，如果输入框末尾**是一个候选上屏的
    /// 整段**，就整段撤掉、把它的拼音原文塞回缓冲 —— 不是逐个删汉字，
    /// 免得用户误选一个字之后要一个个删回去重打。
    fn backspace(&mut self) {
        if self.compose.backspace() {
            return;
        }
        if let Some((text, raw)) = self.committed.last().cloned() {
            if !text.is_empty() && self.input.ends_with(&text) {
                self.committed.pop();
                self.input.truncate(self.input.len() - text.len());
                // 字母原文塞回缓冲，候选会重新出现，用户可以改选
                for c in raw.chars() {
                    self.compose.push(c);
                }
                log::info!("撤掉候选「{text}」，退回字母原文「{raw}」");
                return;
            }
        }
        self.input.pop();
    }

    /// 加载区那六个方块填几个。
    ///
    /// 只有**阅读器里翻页**这件事有可量化的进度（读到第几页 / 共几页）；
    /// 搜索、取历史、开本子这些没法给进度，方块保持空心 —— 主要靠
    /// 「载入第 N 页…」那行字说明现在在干什么。
    fn loading_filled(&self) -> i32 {
        if !self.loading || self.screen != Screen::Reader {
            return 0;
        }
        let total = self.page_total();
        if total == 0 {
            return 0;
        }
        let done = (self.page + 1).min(total);
        ((done as f64 / total as f64) * 6.0).round() as i32
    }

    /// 设置页 / 阅读菜单那一行显示的当前值。
    ///
    /// 行标题本身已经写了「全屏刷新」，所以这里**只给值**，不重复标题 ——
    /// 重复出现两个「全屏刷新」在窄行里会被挤掉。
    fn refresh_label(&self) -> String {
        match self.full_refresh {
            0 => "已关闭".to_string(),
            1 => "每次刷新".to_string(),
            n => format!("每 {n} 次刷新"),
        }
    }

    /// 详情页显示的标题：优先本子标题。
    ///
    /// 详情页那一行小字：**作者 + 车牌号（本子 ID）**。
    ///
    /// 用户要的是「作者用浅灰小字，旁边写上车牌号」。拼成一个字符串
    /// 是因为 Slint 量不了文字宽度 —— 两个 Text 想紧挨着排，
    /// 就得提前知道前一个占多宽，不如在 Rust 拼好。
    ///
    /// 作者缺失时不能留个孤零零的分隔符（jmcomic 有些本子没有作者）。
    fn album_byline(&self) -> String {
        let author = self
            .album
            .as_ref()
            .map(|a| a.author.trim().to_string())
            .unwrap_or_default();
        let id = self
            .album
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_default();

        match (author.is_empty(), id.is_empty()) {
            (true, true) => String::new(),
            (true, false) => format!("ID {id}"),
            (false, true) => author,
            (false, false) => format!("{author}　·　ID {id}"),
        }
    }

    /// 当前这一话的名字（菜单标题用）。取不到就退回总标题。
    fn chapter_name(&self) -> String {
        if let Some(p) = self.photo.as_ref() {
            if let Some(a) = self.album.as_ref() {
                if let Some(c) = a.chapters.iter().find(|c| c.id == p.id) {
                    return chapter_label(c);
                }
            }
        }
        self.reader_title()
    }

    /// 阅读进度：`(当前, 总数)`。长条漫是「屏」，普通页是「页」。
    ///
    /// 抽出来是因为菜单标题和底下状态文字都要用，两边各算一次迟早对不上。
    fn reader_progress(&self) -> (usize, usize) {
        if self.scroll_mode {
            if let Some(h) = self.strip.as_ref().map(|s| s.height() as usize) {
                let step = self.strip_step.max(1);
                let screens = h.div_ceil(step).max(1);
                let cur = (self.strip_y / step + 1).min(screens);
                return (cur, screens);
            }
        }
        (self.page + 1, self.page_total())
    }

    /// 菜单标题右边「第 12 话 · 8 / 24」。
    fn reader_pos(&self) -> String {
        let (cur, total) = self.reader_progress();
        format!("{} · {} / {}", self.chapter_name(), cur, total)
    }

    /// 菜单「章节列表」行右边「共 24 话」。本地单章模式没有目录，不显示。
    fn reader_chapters(&self) -> String {
        match self.album.as_ref().map(|a| a.chapters.len()).unwrap_or(0) {
            0 => String::new(),
            n => format!("共 {n} 话"),
        }
    }

    fn emit(&mut self) {
        // 全屏刷新是**阅读器专有**的：漫画是大面积灰度图、翻一页留一次残影，
        // 值得用整屏闪一下去换干净。而首页/设置/搜索这些是白底黑字的小界面，
        // 本来就不容易攒残影，闪那一下纯粹是打扰。
        self.sync_refresh_policy();

        // 界面或状态一变就打一行日志。
        // 串口日志是这台设备上唯一能从外面看到「屏幕现在是什么」的通道 ——
        // 截图要靠人看，日志能靠脚本查。
        let now = (self.screen, self.status.clone());
        if self.logged.as_ref() != Some(&now) {
            log::info!("界面 {:?} | {}", self.screen, self.status);
            self.logged = Some(now);
        }

        let view = self.view();
        let weak = self.weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(app) = weak.upgrade() {
                apply_view(&app, &view);
            }
        });
    }

    /// 让全屏刷新策略跟着「在不在阅读器」走。
    ///
    /// 只有阅读器里才生效：漫画是大面积灰度图，翻一页留一次残影，
    /// 值得用整屏闪一下去换干净。首页/设置/搜索是白底黑字的小界面，
    /// 本来就不容易攒残影，全刷那一下纯粹是打扰。
    ///
    /// ⚠️ **只在「进出阅读器」的那一刻改一次**，不能每帧都调 ——
    /// `set_interval` 会把计数清零，每帧调的话计数永远攒不起来，
    /// 就再也不会触发全刷了。
    fn sync_refresh_policy(&mut self) {
        let want = self.screen == Screen::Reader;
        if self.refresh_active == Some(want) {
            return;
        }
        let was = self.refresh_active;
        self.refresh_active = Some(want);

        // 关掉的方式是把间隔设成 0（策略里 0 = 从不主动全刷）
        self.refresh
            .set_interval(refresh_interval_for(self.screen, self.full_refresh));

        // 离开阅读器时补一次全刷。
        //
        // 为什么需要：漫画整屏都是灰的，回到目录页/首页时那些界面大部分是白的，
        // 只重画「变了的那块」会留下一大片漫画的影子 —— 而这时候恰恰最明显
        // （白底上任何残留都看得见）。补一次全刷把它清掉。
        if was == Some(true) && !want {
            log::info!("离开阅读器，补一次全屏刷新清掉漫画残影");
            self.refresh.force_next();
        }
    }

    fn say(&mut self, s: impl Into<String>) {
        self.status = s.into();
    }

    // ------------------------------------------------------------ 命令分发

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Boot => self.boot(),
            Cmd::Quit => {
                let _ = slint::invoke_from_event_loop(|| std::process::exit(0));
            }
            Cmd::Back => self.back(),
            Cmd::Retry => {
                // 手动重试：清掉自动发现的结果，重新走一遍完整流程
                self.base = None;
                self.boot();
            }

            // 连接流程（后台线程）的进度与结果
            Cmd::BootStatus { seq, text } => self.apply_boot_status(seq, text),
            Cmd::BootDone {
                seq,
                found,
                skip_saved,
                skip_env,
            } => self.apply_boot(seq, found, skip_saved, skip_env),

            // 搜索请求（后台线程）回来了
            Cmd::SearchDone {
                keyword,
                order,
                page,
                found,
            } => self.apply_search(keyword, order, page, found),

            // 历史记录（后台线程）回来了
            Cmd::HistoryDone(got) => self.apply_history(got),

            // 本子详情（后台线程）回来了
            Cmd::AlbumDone {
                album_id,
                from,
                found,
                tries,
                ms,
            } => self.apply_album(album_id, from, found, tries, ms),

            // 「打开本子并跳进某一话」（后台线程）回来了
            Cmd::AlbumChapterDone {
                album_id,
                photo_id,
                from,
                found,
            } => self.apply_album_then_chapter(album_id, photo_id, from, found),

            // 章节详情（后台线程）回来了
            Cmd::PhotoDone {
                photo_id,
                album_title,
                label,
                found,
            } => self.apply_photo(photo_id, album_title, label, found),

            // 整页图（后台线程）取回并解好了
            Cmd::PageReady {
                key,
                page,
                fit,
                result,
            } => self.apply_page_ready(key, page, fit, result),

            // 详情页封面（后台线程）回来了
            Cmd::AlbumCoverDone { url, thumb } => self.apply_album_cover(url, thumb),

            // 预取的字节到了：只塞缓存，不碰界面
            Cmd::Prefetched { key, bytes } => {
                // 换过章节的话这份就过期了，别往缓存里塞（免得把缓存挤满）
                if key.0 == self.chapter_key() {
                    if self.raw_cache.len() >= RAW_CACHE_MAX {
                        self.raw_cache.clear();
                    }
                    self.raw_cache.insert(key, bytes);
                }
            }

            // 封面线程取好一张，贴到对应的行上。
            Cmd::CoverReady { epoch, idx, url, thumb } => {
                // ⚠️ **过期的直接扔。** 取图是异步的，回来时用户可能已经
                //    换了页 / 换了分栏 / 换了屏。不查代号的话，上一屏的封面
                //    会贴到这一屏的行上（就是 README 第 32 条那类
                //    「多个屏共用一个字段」的坑，只是换成了异步版本）。
                if epoch != self.list_epoch {
                    return;
                }
                // 取失败也记一笔（None），免得每次重建列表都再白试一次
                self.covers.insert(url, thumb.clone());

                let Some(t) = thumb else { return };
                if let Some(row) = self.rows.get_mut(idx) {
                    row.cover = Some(t);
                    self.rows_seq += 1;
                    self.emit();
                }
            }

            Cmd::NavPicked(i) => match i {
                0 => self.open_history(),
                1 => self.open_search(),
                2 => {
                    self.nav_return = self.screen;
                    self.screen = Screen::Home;
                    self.emit();
                }
                3 => self.open_favorites(),
                4 => {
                    self.nav_return = self.screen;
                    self.screen = Screen::Settings;
                    self.emit();
                }
                _ => {}
            },

            Cmd::SetEditServer => {
                // 预填当前地址，改一个数字就行，不用整串重敲
                let prefill = self
                    .base
                    .clone()
                    .or_else(saved_base)
                    .unwrap_or_default();
                self.open_input(
                    InputPurpose::Server,
                    "输入服务器地址（如 192.168.1.91）",
                    prefill,
                );
            }
            Cmd::SetEditRefresh | Cmd::ReaderRefresh => {
                // 入口有两个：设置页那一行，和**阅读器菜单**里 ——
                // 残影是边看边发现的问题，正读到一半觉得糊了，顺手点两下就能调。
                // 从阅读器进来时先把菜单收掉（覆盖层关掉后不该看见菜单还摊着）。
                //
                // 规格 4.6：需要输数字的**在原行上弹小键盘，不做二级页面** ——
                // 所以这里不跳搜索屏，只弹覆盖层、留在当前屏。
                self.reader_menu = false;
                self.open_keypad_overlay(
                    InputPurpose::Refresh,
                    "全屏刷新：每几次全刷一次",
                    self.full_refresh.to_string(),
                    "0 = 关闭",
                );
            }
            Cmd::OpenById => {
                // 主页那个「按本子 ID 打开」——纯数字，弹覆盖层，留在主页
                self.open_keypad_overlay(
                    InputPurpose::AlbumId,
                    "输入本子 ID",
                    String::new(),
                    "纯数字",
                );
            }

            Cmd::KeypadDigit(d) => {
                self.keypad_digit(&d);
                self.emit();
            }
            Cmd::KeypadBackspace => {
                self.keypad_backspace();
                self.emit();
            }
            Cmd::KeypadClear => {
                self.keypad_clear();
                self.emit();
            }
            Cmd::KeypadCancel => {
                // 取消 = 什么都不改，直接关掉覆盖层，留在原来那一屏
                self.keypad_shown = false;
                self.emit();
            }
            Cmd::KeypadOk => self.keypad_ok(),
            Cmd::SetRecheck => {
                // 「点此重测」：丢掉缓存地址，重新走一遍连接流程
                self.base = None;
                self.boot();
            }
            Cmd::SetClearCache => {
                self.covers.clear();
                self.raw_cache.clear();
                self.say("已清除封面与原图缓存");
                self.emit();
            }
            Cmd::PageLeft => {
                if self.screen == Screen::SettingsAbout {
                    self.screen = Screen::Settings;
                    self.emit();
                }
            }
            Cmd::PageRight => {
                if self.screen == Screen::Settings {
                    self.screen = Screen::SettingsAbout;
                    self.emit();
                }
            }

            Cmd::TagActivated(i) => {
                // 点标签 = 按这个标签搜
                if let Some(tag) = self.tag_texts.get(i).cloned() {
                    log::info!("按标签搜索「{tag}」");
                    self.keyword = tag.clone();
                    // 标签搜索是从详情页发起的，不是用户在手打，直接把键盘收起来看结果
                    self.start_search_input(&tag);
                    self.kb_shown = false;
                    self.do_search(1);
                }
            }
            Cmd::RecentPicked(i) => {
                // 点「最近搜索」里的一条 = 把词填回输入框并重搜。
                // 键盘顺手收起来：用户要的是结果，不是继续改词。
                if let Some(kw) = self.recent.items().get(i).cloned() {
                    log::info!("点最近搜索「{kw}」");
                    self.keyword = kw.clone();
                    self.start_search_input(&kw);
                    self.kb_shown = false;
                    self.do_search(1);
                }
            }
            Cmd::RecentClear => {
                self.recent.clear();
                self.say("已清空最近搜索");
                self.emit();
            }
            Cmd::SetSort(i) => {
                if let Some((value, label)) = self.sort_options.get(i).cloned() {
                    if self.order != value {
                        log::info!("排序改为「{label}」({value})");
                        self.order = value;
                        // 换了排序就重搜第 1 页 —— 停在第 3 页看新排序很怪，
                        // 而且那一页的内容跟新排序对不上号
                        self.do_search(1);
                    }
                }
            }

            Cmd::KeyChar(k) => {
                // 键帽和回调都直接给**字符本身**（不是下标），所以不存在
                // 「两边表不同步、按 A 出 B」那个隐患，这里直接用
                self.type_key(&k);
                self.emit();
            }
            Cmd::KeySymbols => {
                // 字母页 <-> 符号页。切页之前把没落定的拼音**原样**落下，
                // 别让用户白打一遍 —— 切页不等于挑字。
                self.raw_commit_text();
                self.kb_symbols = !self.kb_symbols;
                self.kb_seq += 1;
                log::info!("键盘切到{}页", if self.kb_symbols { "符号" } else { "字母" });
                self.emit();
            }
            Cmd::KeySpace => {
                // 拼音缓冲非空时空格 = 落定第一个候选，和手机输入法一致；
                // 空缓冲才真的是打一个空格
                if self.compose.composing() {
                    self.commit_compose();
                } else {
                    self.input.push(' ');
                }
                self.emit();
            }
            Cmd::KeyBackspace => {
                self.backspace();
                self.emit();
            }
            Cmd::KeyConfirm => self.key_confirm(),

            Cmd::ImePick(i) => {
                if self.compose.pick_in_page(i) {
                    self.commit_compose();
                }
                self.emit();
            }
            Cmd::ImePage(d) => {
                self.compose.turn_page(d);
                self.emit();
            }
            Cmd::KbWanted => {
                self.kb_shown = true;
                self.emit();
            }
            Cmd::KbHide => {
                self.kb_shown = false;
                self.emit();
            }

            Cmd::FavTabPicked(i) => {
                self.fav_tab = i;
                self.fav_page = 0;
                if self.screen == Screen::Favorites {
                    self.rebuild_favorites();
                }
                self.emit();
            }

            Cmd::GuidePrev => {
                if self.screen != Screen::Guide {
                    // 主页那个「快速入门」按钮走的就是它：不管从哪进来都从第 1 页看
                    self.screen = Screen::Guide;
                    self.guide_page = 0;
                } else {
                    self.guide_page = (self.guide_page - 1).max(0);
                }
                self.emit();
            }
            Cmd::GuideNext => {
                if self.screen != Screen::Guide {
                    self.screen = Screen::Guide;
                    self.guide_page = 0;
                } else {
                    self.guide_page = (self.guide_page + 1).min(1);
                }
                self.emit();
            }

            Cmd::ReaderMenuToggle => {
                self.reader_menu = !self.reader_menu;
                self.emit();
            }
            Cmd::ReaderFitToggle => {
                // 长条漫是按宽度纵向滚动的，切适屏方式没意义
                if !self.scroll_mode {
                    self.fit_mode = self.fit_mode.toggled();
                    log::info!("适屏方式切换为 {}", self.fit_mode.label());
                    // 重画当前页（图还在缓存里，不用重新下载）
                    let p = self.page;
                    self.load_page(p);
                }
            }
            Cmd::ReaderPrev => self.turn(-1),
            Cmd::ReaderNext => self.turn(1),
            Cmd::ReaderGotoPage => {
                // 跳页也是纯数字，走覆盖式小键盘（规格 4.6），**留在阅读器**不跳屏
                self.reader_menu = false;
                let total = self.page_total();
                self.open_keypad_overlay(
                    InputPurpose::Page,
                    "跳转到第几页",
                    String::new(),
                    &format!("共 {total} 页"),
                );
            }
            Cmd::ReaderOpenChapters => {
                // 阅读器菜单里的「章节列表」= 回本子目录
                if self.album.is_some() {
                    self.reader_menu = false;
                    self.album_return = Screen::Reader;
                    self.screen = Screen::Album;
                    self.rebuild_album();
                    self.emit();
                } else {
                    self.say("本地测试模式没有目录");
                    self.emit();
                }
            }
            Cmd::ReaderStarToggle => {
                self.toggle_current_chapter_fav();
                self.emit();
            }
            Cmd::ReaderExit => {
                // 「退出阅读」和按返回是同一条路：回目录（有目录时）
                self.back();
            }
            Cmd::ReaderHintDismiss => {
                mark_hint_seen();
                self.reader_hint = false;
                self.emit();
            }

            Cmd::ListPrev => self.list_page(-1),
            Cmd::ListNext => self.list_page(1),
            Cmd::Activate(i) => self.activate(i),
            Cmd::StarHit(i) => self.star_hit(i),
        }
    }

    // ------------------------------------------------------------ 启动

    /// 连上服务端。
    ///
    /// 流程：
    ///   1. 按优先级列出候选地址（`--server` > 用户手填 > `MANGA_SERVER`）
    ///   2. **逐个探活，取第一个真的连得上的**
    ///   3. 一个都不通 → 局域网 UDP 广播发现，找到再探一次
    ///
    /// 手填的排在环境变量前面，是因为**用户在界面上明确输入过**是更强的意愿；
    /// 环境变量只是打包时给的默认值。
    ///
    /// 关键点是「逐个探活」而不是「有就用」：`server.txt` 里存着过期地址、
    /// 而 `MANGA_SERVER` 是好的时候，先探活才能一次就选对，不用用户手动重试。
    fn boot(&mut self) {
        // 本地单章模式：没有服务端，直接进阅读器
        if self.local.is_some() {
            self.enter_reader();
            self.load_page(0);
            return;
        }

        // ⚠️ **整个发现过程在后台线程上跑**（[`discover_server`]）。
        //
        // 这段最多要试若干候选地址 + 一次 3 秒的广播，全落空就是十几秒。
        // 原来在 worker 上同步做，等于点一下「重试」/「重新寻找服务端」
        // 就把应用冻十几秒、期间点击全排队 —— 和取图那条路是同一个毛病。
        //
        // `boot_seq` 是这一轮的代号：用户可能连点两次重试，或者在
        // 设置里改完地址又改一次。只有最新一轮的结果才允许上屏，
        // 否则旧的那一轮晚回来会把新地址覆盖掉。
        self.boot_seq += 1;
        let seq = self.boot_seq;

        self.say("正在连接…");
        self.loading = false;
        self.emit();

        let known = self.base.clone();
        let skip_saved = self.skip_saved;
        let skip_env = self.skip_env;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let (found, skip_saved, skip_env) =
                discover_server(known, skip_saved, skip_env, &tx, seq);
            let _ = tx.send(Cmd::BootDone {
                seq,
                found,
                skip_saved,
                skip_env,
            });
        });
    }

    /// 后台线程报来的一句进度（「正在连接（试 N 个候选地址）…」之类）。
    fn apply_boot_status(&mut self, seq: u64, text: String) {
        // 过期的那一轮就别刷屏了
        if seq != self.boot_seq {
            return;
        }
        self.loading = false;
        self.say(text);
        self.emit();
    }

    /// 连接流程跑完了。
    fn apply_boot(
        &mut self,
        seq: u64,
        found: Option<(String, api::Info)>,
        skip_saved: bool,
        skip_env: bool,
    ) {
        // 用户又点了一次重试 / 改了地址 —— 这一轮作废
        if seq != self.boot_seq {
            return;
        }

        self.skip_saved = skip_saved;
        self.skip_env = skip_env;

        if let Some((base, info)) = found {
            self.on_connected(base, info);
            return;
        }

        // ---- 一个都没成 ----
        // 走独立的出错页（规格 4.11）：把地址、状态、上次成功摊开，
        // 用户不用去翻日志也能判断问题出在哪一段。
        //
        // 先清掉「之前连上的地址」：它的探活刚失败了，留着只会
        // 让出错页显示一个其实不通的地址。
        self.base = None;
        self.screen = Screen::Error;
        self.display = None;
        self.image_seq += 1;
        self.err_title = "无法连接服务器".to_string();
        self.err_desc = "确认电脑上 jm_kindle_raw.py 正在运行、Kindle 和电脑连同一个 WiFi，\
                         填的是电脑的局域网 IP（192.168.x.x）。点「重试」再试一次，\
                         或进「设置」手填地址。"
            .to_string();
        self.err_addr = self
            .base
            .clone()
            .or_else(saved_base)
            .map(|b| strip_scheme(&b).to_string())
            .unwrap_or_else(|| "(自动发现)".to_string());
        self.err_state = "连接失败".to_string();
        self.err_last = saved_base()
            .map(|b| strip_scheme(&b).to_string())
            .unwrap_or_else(|| "—".to_string());
        self.say("没找到服务端");
        self.emit();
    }

    /// 探活成功后统一走这里。
    fn on_connected(&mut self, base: String, info: api::Info) {
        // 用服务端自己声明的默认排序，别在客户端另造一份
        if !info.default_sort.is_empty() {
            self.order = info.default_sort.clone();
        }
        // 排序选项同样由服务端给。空表 = 旧版服务端，界面上不显示排序按钮。
        self.sort_options = info
            .sort_options
            .iter()
            .map(|o| (o.value.clone(), o.label.clone()))
            .collect();
        if !self.sort_options.is_empty() {
            let names: Vec<&str> = self.sort_options.iter().map(|(_, l)| l.as_str()).collect();
            log::info!("服务端支持 {} 种排序：{}", names.len(), names.join(" / "));
        }
        log::info!(
            "已连接 {} v{}，屏幕基准 {}x{}",
            base,
            info.version,
            info.screen.width,
            info.screen.height
        );
        self.base = Some(base.clone());
        self.say(format!("已连接 {base}  ·  服务端 v{}", info.version));
        // 首屏是**历史记录**（规格定的），连上就把它拉出来铺上
        self.screen = Screen::History;
        self.open_history();
    }

    // ------------------------------------------------------------ 搜索 / 输入

    /// 打开「输入面」。v5 没有独立键盘屏了，搜索页兼作所有文字/数字输入 ——
    /// 填服务器地址、填全屏刷新间隔、跳页都走这里，只是确认键的去向不同。
    fn open_input(&mut self, purpose: InputPurpose, title: &str, prefill: String) {
        self.nav_return = self.screen;
        self.input = prefill;
        self.input_purpose = purpose;
        // 每次打开都是干净的：上一次没落定的拼音、上一次的候选段都不该跟过来
        self.committed.clear();
        self.compose.clear();
        self.kb_shown = true;
        self.kb_symbols = false;
        self.kb_seq += 1;
        // 输入页不显示列表结果：清掉，免得底下冒出上一个屏的残留
        self.rows.clear();
        self.search_ids.clear();
        self.search_page = 1;
        self.search_page_count = 1;
        self.has_prev = false;
        self.has_next = false;
        self.list_info.clear();
        self.list_sub = title.to_string();
        self.rows_seq += 1;
        self.screen = Screen::Search;
        self.emit();
    }

    /// 切到搜索标签页。输入框预填上次的关键词，键盘竖起，结果先清掉。
    ///
    /// `rows` 是历史/搜索共用的，不清的话会看到上一个屏（历史）的列表
    /// —— 这正是当年 `rebuild_album` 那个「多屏共用一个字段」的坑的变种。
    fn open_search(&mut self) {
        self.nav_return = self.screen;
        self.start_search_input(&self.keyword.clone());
        self.kb_shown = true;
        self.rows.clear();
        self.search_ids.clear();
        self.search_page = 1;
        self.search_page_count = 1;
        self.has_prev = false;
        self.has_next = false;
        self.list_info.clear();
        self.list_sub = "输入关键词开始搜索".to_string();
        self.rows_seq += 1;
        self.search_fresh = true;
        self.screen = Screen::Search;
        self.emit();
    }

    /// 进入「搜索」用途的干净输入状态（输入框预填 given）。
    /// 搜索页本身不清列表 —— 调用方（标签搜索）紧接着就会用结果覆盖它。
    fn start_search_input(&mut self, prefill: &str) {
        self.input_purpose = InputPurpose::Search;
        self.input = prefill.to_string();
        self.committed.clear();
        self.compose.clear();
        self.kb_symbols = false;
        self.kb_seq += 1;
    }

    fn key_confirm(&mut self) {
        // 拼音还没落定的，**原样**落进输入框再搜。
        //
        // 这里以前是 `commit_compose()`（落第一个候选），改掉的原因：
        // 用户没点候选就等于他没要中文，那串字母就是他要搜的英文。
        // 旧写法在拼音模式下有两条相反方向的毛病 ——
        // `naruto` / `kindle` 这类表里没有的音，`take()` 返回 `None`，
        // 输入框还是空的，按确定只弹一句「还没输入内容」，**英文根本搜不了**；
        // 而 `one` 恰好是合法拼音，就被无声无息换成了「哦呢」。
        self.raw_commit_text();
        let text = self.input.trim().to_string();

        // 服务器地址这一项允许留空 —— 留空的意思是「清除手填的地址，
        // 回到自动发现」。其他用途留空没意义。
        if self.input_purpose == InputPurpose::Server {
            self.save_server_and_reconnect(&text);
            return;
        }

        if text.is_empty() {
            self.say("还没输入内容");
            self.emit();
            return;
        }
        match self.input_purpose {
            InputPurpose::Search => {
                self.keyword = text;
                // 按「搜索」= 收起键盘，结果区从 490px 撑到 1128px（规格 4.2）
                self.kb_shown = false;
                self.do_search(1);
            }
            InputPurpose::Refresh => match text.parse::<u32>() {
                Ok(v) => self.apply_full_refresh(v.min(FULL_REFRESH_MAX)),
                Err(_) => {
                    // 留在输入页，不用重敲一遍
                    self.say("请输入数字：0 = 关闭，或者每 N 次刷新全刷一次");
                    self.emit();
                }
            },
            // 下面三个纯数字用途在 v5 都改走覆盖式小键盘了，
            // 搜索屏这条分支只是兜底（正常到不了）。
            InputPurpose::Page => {
                self.screen = self.nav_return;
                self.reader_menu = false;
                let total = self.page_total();
                match text.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= total => self.load_page(n - 1),
                    _ => {
                        self.say(format!("请输入 1 到 {total} 之间的页码"));
                        self.emit();
                    }
                }
            }
            InputPurpose::AlbumId => self.open_album(&text),
            InputPurpose::Server => unreachable!("上面已经处理"),
        }
    }

    /// 应用并保存全刷间隔，然后立刻强制全刷一次让用户看到效果。
    ///
    /// ⚠️ **不改 `screen`、不碰 `nav_return`。** 现在它只从小键盘覆盖层调用，
    /// 覆盖层关掉之后用户就该留在原来那一屏（设置页 / 阅读器）。
    fn apply_full_refresh(&mut self, interval: u32) {
        self.full_refresh = interval;
        if let Err(e) = save_refresh_interval(interval) {
            // 存不下不影响本次生效，只是下次启动会回到默认值
            log::warn!("保存全刷间隔失败：{e}");
        }

        let msg = if interval == 0 {
            "已关闭自动全屏刷新（残影会一直累积）".to_string()
        } else {
            format!("已设置：阅读器里每 {interval} 次刷新做一次全屏刷新")
        };
        log::info!("全屏刷新间隔 = {interval}");
        self.say(msg);

        // 置空让 sync 重新同步一次：改设置时人在覆盖层上，不是在阅读器，
        // 直接调 set_interval 会绕过「只在阅读器生效」那道门。
        self.refresh_active = None;
        self.emit();

        // 立刻来一次全刷 —— 用户改这个设置就是想看屏幕变干净，
        // 等 N 页之后再刷等于没有反馈。
        //
        // 放在 emit 之后：emit 只是把重绘排进 UI 线程的事件循环，
        // 真正画出来还在后面，所以这个标志来得及被那一次绘制消费掉。
        self.refresh.force_next();
    }

    // ------------------------------------------------------------ 数字小键盘覆盖层

    /// 弹出数字小键盘覆盖层。
    ///
    /// **留在当前屏**，只切 `keypad_shown`：底下的界面一个像素都不动（规则 ⑨），
    /// 关掉就回到原样。所以这里**绝不能改 `screen`、也绝不能碰 `nav_return`**。
    fn open_keypad_overlay(
        &mut self,
        purpose: InputPurpose,
        title: &str,
        prefill: String,
        note: &str,
    ) {
        self.keypad_purpose = purpose;
        self.keypad_title = title.to_string();
        self.keypad_value = prefill;
        self.keypad_note = note.to_string();
        self.keypad_shown = true;
        log::info!("弹出数字小键盘：{title}");
        self.emit();
    }

    /// 覆盖层是否开着（点「确定 / 取消」之外的地方不该误触，界面自己会吃掉）。
    fn close_keypad(&mut self) {
        self.keypad_shown = false;
        self.emit();
    }

    /// 按一个数字键。**只收单个 ASCII 数字**（回调给的就是单字符），
    /// 到上限就忽略 —— 防止数字串无限长把输入框和光标撑爆。
    fn keypad_digit(&mut self, d: &str) {
        let mut it = d.bytes();
        let (Some(b), None) = (it.next(), it.next()) else {
            return; // 空串或不止一个字符，一律忽略
        };
        if !b.is_ascii_digit() {
            return;
        }
        if self.keypad_value.chars().count() >= KEYPAD_MAX_DIGITS {
            return;
        }
        self.keypad_value.push(b as char);
    }

    fn keypad_backspace(&mut self) {
        self.keypad_value.pop();
    }

    fn keypad_clear(&mut self) {
        self.keypad_value.clear();
    }

    /// 「确定」。按用途分派；只有非法输入会留在覆盖层上并给一行反馈。
    fn keypad_ok(&mut self) {
        let action = keypad_action(
            self.keypad_purpose,
            &self.keypad_value,
            self.page_total(),
        );
        match action {
            KeypadAction::Close => self.close_keypad(),
            KeypadAction::Invalid(msg) => {
                // **不关**覆盖层：让用户改完再确定，不用重新弹一次
                self.keypad_note = msg.to_string();
                self.say(msg);
                self.emit();
            }
            KeypadAction::FullRefresh(n) => {
                self.keypad_shown = false;
                self.apply_full_refresh(n);
            }
            KeypadAction::GotoPage(n) => {
                self.keypad_shown = false;
                self.load_page(n - 1);
            }
            KeypadAction::OpenAlbum(id) => {
                self.keypad_shown = false;
                self.open_album(&id);
            }
        }
    }

    /// 保存用户手填的服务器地址并重连。
    ///
    /// 空字符串 = 清除，下次走自动发现。
    fn save_server_and_reconnect(&mut self, raw: &str) {
        let normalized = api::normalize_base(raw);

        if let Err(e) = save_base(&normalized) {
            // 存不下去也不是致命的 —— 本次仍然用这个地址连，只是下次要重填
            log::warn!("保存服务器地址失败：{e}");
        }

        if normalized.is_empty() {
            self.say("已清除手填地址，改为自动发现…");
            log::info!("清除手填服务器地址，回到自动发现");
        } else {
            self.say(format!("已保存 {normalized}，正在连接…"));
            log::info!("保存服务器地址 {normalized}");
        }

        self.base = None;
        // ⚠️ **必须清掉这两个跳过标记。**
        //
        // 它们是「上次所有候选地址都探不通，别再白试了」的标记，一旦置位，
        // `candidate_bases()` 就返回空列表，于是直接跳到广播发现。
        //
        // 而用户手填地址是**明确的意图**，必须真的去试它。不清的话现象是：
        // 输了地址、点了确定，界面显示「已保存…正在连接」，然后**立刻**
        // 跳到「正在局域网里找服务端」—— 新地址压根没被探过。
        // 用户看到的是「填了地址还是连不上」，反复输也没用，只能重启应用。
        //
        // 真机日志里踩到的就是这个：
        //   保存服务器地址 http://192.168.2.59:8001
        //   界面 Home | 已保存 http://192.168.2.59:8001，正在连接…
        //   改用局域网广播发现          ← 中间少了「候选地址 1 个，逐个探活」
        self.skip_saved = false;
        self.skip_env = false;

        self.screen = Screen::History;
        self.emit();
        self.boot();
    }

    /// 发起一次搜索。
    ///
    /// ⚠️ **HTTP 请求本身在后台线程上发**，这里只做两件事：记下这次请求的
    ///    身份、立刻给用户一个「在搜了」的反馈。原因见 [`Cmd::SearchDone`]。
    fn do_search(&mut self, page: u32) {
        let Some(base) = self.base.clone() else {
            self.say("还没连上服务端");
            self.emit();
            return;
        };

        let keyword = self.keyword.clone();
        let order = self.order.clone();
        // 记下发起时在哪一屏：回来时用户还在这儿才作数
        self.search_req = Some((keyword.clone(), order.clone(), page, self.screen));

        // 第 1 页才算「一次新的搜索」—— 翻页、换排序不该把词再记一遍
        // （去重会把它挪到最前，本来也无害，但没必要白写一次闪存）。
        if page == 1 {
            self.recent.add(&keyword);
        }
        // 搜过之后就不再是「刚打开」了，最近搜索让位给结果（哪怕是空结果）
        self.search_fresh = false;

        // 立刻给反馈。
        //
        // ⚠️ 原来这一整段**什么提示都没有**：点了搜索之后屏幕上什么都不变，
        //    用户以为没点上就会反复点，那几下全堆在队列里，等搜索结果回来
        //    连着执行（就是「跟末影珍珠一样」的来源）。
        self.loading = true;
        self.loading_text = "搜索中…".into();
        // 结果区也留一句话：点了「最近搜索」之后列表是空的，
        // 不写点什么的话那几秒里看着像没点上（阅读器那边有加载面板，
        // 搜索页没有 —— 这里用下面那行空态文字当反馈）。
        if page == 1 {
            self.list_sub = format!("正在搜索「{keyword}」…");
        }
        self.emit();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let found =
                api::retry(3, || api::search(&base, &keyword, &order, page)).map(|(r, _)| r);
            let _ = tx.send(Cmd::SearchDone {
                keyword,
                order,
                page,
                found,
            });
        });
    }

    /// 搜索结果回来了：贴上屏，或者报错。
    fn apply_search(
        &mut self,
        keyword: String,
        order: String,
        page: u32,
        found: Result<api::SearchResult, api::ApiError>,
    ) {
        // ⚠️ **身份对不上就丢弃。** 这是异步化的代价：请求发出去之后，用户
        //    可能已经改了词、换了排序、翻到别的页 —— 不校验的话，
        //    「搜 A 的结果」会贴到「搜 B 的界面」上。
        if self.search_req.as_ref() != Some(&(keyword.clone(), order.clone(), page, self.screen))
        {
            return;
        }
        self.search_req = None;

        let base = self.base.clone();
        match found {
            Ok(r) => {
                self.search_page = r.page.max(1);
                // 上游没给总数时 page_count 是 null —— 那就只能知道「当前这一页」，
                // 把总页数当成当前页，于是「下一页」按钮不可用，不会翻进空页。
                self.search_page_count = r.page_count.unwrap_or(self.search_page).max(1);
                self.search_ids = r.items.iter().map(|it| it.id.clone()).collect();

                // 先把文字铺上去，封面随后一张张补
                self.rows = r
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| RowData {
                        label: it.title.clone(),
                        sub: format!("本子 ID {}", it.id),
                        sub_label: String::new(),
                        idx: i as i32,
                        cover: None,
                        // 搜索结果也能直接点星收藏（空心 = 还没收藏）
                        star: album_star(&self.favs, &it.id),
                    })
                    .collect();
                // 结果为空时居中显示一句话，不然那块地方是空的
                self.list_sub = if self.rows.is_empty() {
                    format!("没有搜到「{}」", r.keyword)
                } else {
                    String::new()
                };
                // 同上：别动 col_left/col_right（那是目录页的）
                self.screen = Screen::Search;
                self.loading = false;
                self.rows_seq += 1;

                match r.total {
                    Some(t) => self.say(format!("「{}」共 {} 个结果", r.keyword, t)),
                    None => self.say(format!("搜索「{}」", r.keyword)),
                }
                // 翻页状态和页码统一从 restore_pager 算 —— 三处各写各的
                // 正是「返回列表时页码不刷新」那类 bug 的温床
                self.restore_pager();
                self.emit();

                let covers: Vec<(usize, String)> = r
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| (i, it.cover.clone()))
                    .collect();
                if let Some(base) = base {
                    self.load_covers(&base, covers);
                }
            }
            Err(e) => {
                self.loading = false;
                // 也写进结果区：直接跑到这里时列表可能是空的，
                // 只在日志/状态里说一句用户看不见。
                self.list_sub = format!("搜索失败：{e}");
                self.say(format!("搜索失败：{e}"));
                self.emit();
            }
        }
    }

    /// 封面一张张补上，每拿到一张就重发一次 View。
    ///
    /// 电子墨水屏上「先出标题、图慢慢跟上」比整页卡着等所有封面要舒服得多；
    /// 而且服务端首次取封面可能要现下载，全等完可能十几秒。
    fn load_covers(&mut self, base: &str, targets: Vec<(usize, String)>) {
        // 新的一批：代号 +1。上一批还没回来的结果就此作废（见 Cmd::CoverReady）。
        self.list_epoch += 1;
        let epoch = self.list_epoch;

        // 先扫一遍缓存。命中的当场贴上去 —— 换页/切分栏再回来时，
        // 封面是立刻出现的，不会再「空框一下、图再慢慢补」。
        let mut pending: Vec<(usize, String)> = Vec::new();
        let mut hit_any = false;
        for (idx, url) in targets {
            if url.is_empty() {
                continue;
            }
            match self.covers.get(&url) {
                Some(cached) => {
                    if let Some(t) = cached {
                        if let Some(row) = self.rows.get_mut(idx) {
                            row.cover = Some(t.clone());
                            hit_any = true;
                        }
                    }
                }
                // 没缓存过 —— 交给后台线程去取
                None => pending.push((idx, url)),
            }
        }
        if hit_any {
            self.rows_seq += 1;
            self.emit();
        }

        if pending.is_empty() {
            return;
        }

        // ⚠️ **取图必须在别的线程上做。**
        //
        // 原来是就地同步取的：worker 线程被 HTTP + 解码占住好几秒，
        // 那期间用户点的东西全堵在 channel 里，等封面取完一次性全放出来
        // （用户形容「跟末影珍珠一样」）。
        //
        // 现在 worker 发一条就立刻空出来，接着处理下一次点击；
        // 每取好一张从 `tx` 送一条 CoverReady 回来，worker 像处理
        // 普通命令那样把它贴到对应的行上。
        let base = base.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            for (idx, url) in pending {
                // 统一按 THUMB_MAX 解码：搜索页的三列网格要放大到 340px，
                // 列表行再靠 Slint 缩小显示。尺寸统一，缓存才不会「换屏就糊」。
                let thumb = api::fetch_bytes(&base, &url)
                    .ok()
                    .flatten()
                    .and_then(|b| render::decode_thumb(&b, THUMB_MAX, THUMB_MAX).ok());
                // 送不进去说明 worker 已经退出了（程序在关），别再白取图
                if tx.send(Cmd::CoverReady { epoch, idx, url, thumb }).is_err() {
                    return;
                }
            }
        });
    }

    // ------------------------------------------------------------ 历史

    /// 打开历史记录。
    ///
    /// ⚠️ 请求在后台线程上发（为什么见 [`Cmd::SearchDone`] 那段）。
    ///    同时**先切屏、先给提示**：原来是「取回来才切屏」，点底栏
    ///    「历史」之后要盯着搜索页干等几秒，跟没点上一样。
    fn open_history(&mut self) {
        let Some(base) = self.base.clone() else {
            self.say("还没连上服务端");
            self.emit();
            return;
        };

        self.screen = Screen::History;
        // 这一屏的列表要重铺，先清掉上一屏残留的行。
        // ⚠️ **只清 rows** —— col_left/col_right 是目录页的数据，
        //    历史页读不到它们，清掉只会把目录毁掉（README 第 32 条那类坑）。
        self.rows.clear();
        self.say("正在取观看记录…");
        self.list_sub = "正在取观看记录…".into();
        self.rows_seq += 1;
        self.loading = true;
        self.loading_text = "取观看记录…".into();
        self.emit();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let got = api::retry(3, || api::history(&base)).map(|(h, _)| h);
            let _ = tx.send(Cmd::HistoryDone(got));
        });
    }

    /// 历史记录回来了。
    fn apply_history(&mut self, got: Result<Vec<api::HistoryItem>, api::ApiError>) {
        // ⚠️ **用户已经离开历史页就丢弃。** 否则这份数据晚回来时会把
        //    正在看别的页面的用户**硬拽回历史页** —— 异步化之后
        //    最容易出的就是这种「界面自己跳走」的毛病。
        if self.screen != Screen::History {
            return;
        }
        self.loading = false;
        match got {
            Ok(items) => {
                self.history = items;
                self.history_page = 0;
                self.rebuild_history();
                self.emit();
            }
            Err(e) => {
                self.say(format!("取观看记录失败：{e}"));
                self.emit();
            }
        }
    }

    fn rebuild_history(&mut self) {
        let total = self.history.len();
        let pages = total.div_ceil(HISTORY_PER_PAGE).max(1);
        if self.history_page >= pages {
            self.history_page = pages - 1;
        }

        let start = self.history_page * HISTORY_PER_PAGE;
        self.rows = self
            .history
            .iter()
            .enumerate()
            .skip(start)
            .take(HISTORY_PER_PAGE)
            .map(|(i, h)| RowData {
                label: h.title.clone(),
                sub: format!("本子 ID {}", h.id),
                sub_label: String::new(),
                idx: i as i32,
                cover: None,
                // 历史行也能直接点星收藏
                star: album_star(&self.favs, &h.id),
            })
            .collect();
        // 不要去 clear col_left/col_right —— 那是目录页的数据，历史页读不到它们，
        // 清掉只会把目录的内容毁掉（和 rebuild_album 里那个 rows.clear() 同源）。
        let (p, n, info) = pager(self.history_page, pages);
        self.has_prev = p;
        self.has_next = n;
        self.list_info = info;
        self.rows_seq += 1;

        // 历史页的副标题（标题「历史记录」是界面写死的，只有副标题能变）
        if total == 0 {
            self.list_sub = "服务器上还没有观看记录".to_string();
            self.say("服务器上还没有观看记录");
        } else {
            self.list_sub = format!("最近观看  ·  共 {total} 条");
            self.say("最近观看");
        }

        // ---- 封面 ----
        //
        // ⚠️ 这里是「历史页永远是一排空框」的根因：上面那些行 `cover: None`，
        //    而**只有搜索页调了 load_covers** —— 历史页建完 rows 就没人管封面了。
        //
        // `/api/history` 只回 {id, title}，没有封面字段；但服务端有
        // `/cover/<album_id>`，而历史条目存的就是相册 id，直接拼出来取即可。
        //
        // 索引用**页内下标**（不是 RowData.idx）—— idx 存的是整份历史的绝对下标
        // （点行时要拿它去索引 self.history），拿它去 load_covers 里索引 rows
        // 会错位到别的行上，把封面贴到隔壁去。
        //
        // ⚠️ 取图之前先 emit 一次：load_covers 是**阻塞**的（一张一次网络请求），
        //    顺序反了就是「列表先卡着等 8 张封面下载完才显示」。
        self.emit();
        let targets: Vec<(usize, String)> = self
            .history
            .iter()
            .skip(start)
            .take(HISTORY_PER_PAGE)
            .enumerate()
            .map(|(i, h)| (i, format!("/cover/{}", h.id)))
            .collect();
        if let Some(base) = self.base.clone() {
            self.load_covers(&base, targets);
        }
    }

    // ------------------------------------------------------------ 收藏

    /// 打开收藏页（底栏第 4 项）。
    fn open_favorites(&mut self) {
        self.nav_return = self.screen;
        self.fav_page = 0;
        self.screen = Screen::Favorites;
        self.rebuild_favorites();
        self.emit();
    }

    /// 按当前分栏（作品 / 单话）重铺收藏列表。
    ///
    /// 收藏页的内容**全部已收藏**，所以星标恒为实心（2）。
    fn rebuild_favorites(&mut self) {
        let list = if self.fav_tab == 1 {
            self.favs.chapters()
        } else {
            self.favs.albums()
        };
        let total = list.len();
        let pages = total.div_ceil(HISTORY_PER_PAGE).max(1);
        if self.fav_page >= pages {
            self.fav_page = pages - 1;
        }
        let start = self.fav_page * HISTORY_PER_PAGE;
        self.rows = list
            .iter()
            .enumerate()
            .skip(start)
            .take(HISTORY_PER_PAGE)
            .map(|(i, f)| {
                // 单话收藏显示**两行**：上面是所属本子的标题，下面是「第 N 话」。
                //
                // 单话自己的 `title` 只是「第 1 話」这种短标签，单看它不知道
                // 是哪个本子；而本子标题本来就不在 `title` 里，所以收藏时
                // 单独存了一份（见 favorites::Fav::album_title）。
                //
                // ⚠️ 老数据没有 `album_title`（这个字段是后加的，真机上那份
                //    favorites.json 就是），此时**退回单行**显示单话自己的
                //    title —— 宁可少一行，也不能显示成空标题。
                let (label, sub_label) = if f.is_album() || f.album_title.is_empty() {
                    (f.title.clone(), String::new())
                } else {
                    (f.album_title.clone(), f.title.clone())
                };
                RowData {
                    label,
                    sub: if f.is_album() {
                        format!("本子 ID {}", f.id)
                    } else {
                        format!("单话 ID {}", f.id)
                    },
                    idx: i as i32,
                    cover: None,
                    star: 2,
                    sub_label,
                }
            })
            .collect();
        self.fav_items = list;
        let (p, n, info) = pager(self.fav_page, pages);
        self.has_prev = p;
        self.has_next = n;
        self.list_info = info;
        self.rows_seq += 1;
        self.say(if total == 0 {
            "还没有收藏".to_string()
        } else {
            format!("收藏了 {total} 条")
        });

        // ---- 封面 ----
        // 本子取自己的封面（/cover/<本子id>）。
        //
        // **单话也取封面，用的是「所属本子」的** —— 服务端没有单话封面这个
        // 接口，而 jmcomic 的章节本来就没有独立封面，本子封面就是这一话的封面。
        // （原来这里 filter(is_album) 把单话整个跳过了，所以单话行左边
        //   一直是个空框 —— 用户报的「单话看不到封面图」。）
        //
        // 同一本子的多话会请求同一个 URL，但 load_covers 按 URL 缓存，
        // 第二话起直接命中缓存，不会重复发请求。
        //
        // 同样先 emit 再取图（load_covers 阻塞，别让列表干等）。
        self.emit();
        let targets: Vec<(usize, String)> = self
            .fav_items
            .iter()
            .skip(start)
            .take(HISTORY_PER_PAGE)
            .enumerate()
            .filter_map(|(i, f)| {
                let id = if f.is_album() { &f.id } else { &f.album_id };
                // album_id 为空 = 没有可用的本子（老数据或本地测试造的假数据），
                // 没得取就跳过，别去请求 /cover/ 那种空 id。
                if id.is_empty() {
                    None
                } else {
                    Some((i, format!("/cover/{id}")))
                }
            })
            .collect();
        if let Some(base) = self.base.clone() {
            self.load_covers(&base, targets);
        }
    }

    /// 点收藏页一行：作品进详情；单话先打开所属本子再跳进那一话。
    fn open_fav(&mut self, fav: &favorites::Fav) {
        if !fav.is_album() && !fav.album_id.is_empty() {
            self.open_album_then_chapter(&fav.album_id, &fav.id);
        } else {
            self.open_album(&fav.id);
        }
    }

    /// 打开某个本子并直接跳到指定的那一话（收藏的单话入口用）。
    ///
    /// 必须先把 album 取回来 —— `open_chapter` 要靠 `self.album` 找章节、
    /// 算「下一话」，光有一个 photo id 是不够的。
    fn open_album_then_chapter(&mut self, album_id: &str, photo_id: &str) {
        let Some(base) = self.base.clone() else {
            self.say("还没连上服务端");
            self.emit();
            return;
        };

        let from = self.screen;
        self.album_req = Some((album_id.to_string(), from));
        self.loading = true;
        self.loading_text = "打开收藏…".into();
        self.emit();

        let id = album_id.to_string();
        let pid = photo_id.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let found = api::retry(3, || api::album(&base, &id)).map(|(a, _)| a);
            let _ = tx.send(Cmd::AlbumChapterDone {
                album_id: id,
                photo_id: pid,
                from,
                found,
            });
        });
    }

    /// 「打开本子并跳进某一话」的结果回来了。
    fn apply_album_then_chapter(
        &mut self,
        album_id: String,
        photo_id: String,
        from: Screen,
        found: Result<api::Album, api::ApiError>,
    ) {
        // 同 apply_album：身份对不上、或者用户已经走开了，就丢弃
        if self.album_req.as_ref() != Some(&(album_id.clone(), from)) || self.screen != from {
            return;
        }
        self.album_req = None;

        match found {
            Ok(a) => {
                let pos = a.chapters.iter().position(|c| c.id == photo_id);
                self.album_return = from;
                // 详情页那几个字段一并铺好 —— 从这一话退回目录时要显示正确的
                // 标题/标签/封面，不能留着上一个本子的。
                self.album_title = a.title.clone();
                self.album_page = 0;
                self.tag_texts = a.tags.clone();
                self.album_tags = layout_tags(&a.tags);
                self.album_cover = None;
                self.album = Some(a);
                match pos {
                    Some(i) => self.open_chapter(i),
                    None => {
                        // 本子里找不到这一话（可能被删了），退回目录
                        self.screen = Screen::Album;
                        self.rebuild_album();
                        self.emit();
                    }
                }
            }
            Err(e) => {
                self.say(format!("打开收藏失败：{e}"));
                self.emit();
            }
        }
    }

    /// 点列表行右侧星标 = 收藏 / 取消收藏。各屏含义不同：
    ///   收藏页 → 从收藏里删掉这一条
    ///   搜索 / 历史 → 收藏这个本子
    ///   目录 → 收藏这一话
    fn star_hit(&mut self, idx: i32) {
        if idx < 0 {
            return;
        }
        let i = idx as usize;
        match self.screen {
            Screen::Favorites => {
                let Some(fav) = self.fav_items.get(i).cloned() else {
                    return;
                };
                // 收藏页点星一律是「取消收藏」—— 列表里全是已收藏的东西
                self.favs.toggle(fav);
                self.save_favs();
                self.rebuild_favorites();
                self.emit();
            }
            Screen::Search => {
                let Some(id) = self.search_ids.get(i).cloned() else {
                    return;
                };
                let title = self
                    .rows
                    .get(i)
                    .map(|r| r.label.clone())
                    .unwrap_or_default();
                let now = self.favs.toggle(favorites::Fav::album(id, title));
                if let Some(row) = self.rows.get_mut(i) {
                    row.star = if now { 2 } else { 1 };
                }
                self.save_favs();
                self.rows_seq += 1;
                self.emit();
            }
            Screen::History => {
                let Some((id, title)) = self
                    .history
                    .get(i)
                    .map(|h| (h.id.clone(), h.title.clone()))
                else {
                    return;
                };
                let now = self.favs.toggle(favorites::Fav::album(id, title));
                if let Some(row) = self.rows.get_mut(i) {
                    row.star = if now { 2 } else { 1 };
                }
                self.save_favs();
                self.rows_seq += 1;
                self.emit();
            }
            Screen::Album => {
                // 目录行的 idx 就是章节下标；点星收藏这一话。
                // **当前正读的那一话**要顺带同步 reader_starred，否则菜单里
                // 「收藏本话」的两态会和目录上的星对不上。
                //
                // 本子标题也一起存下来 —— 收藏页单话行要显示「本子标题 /
                // 第 N 话」两行，见 favorites::Fav::album_title。
                let Some((cid, album_id, album_title, label)) =
                    self.album.as_ref().and_then(|a| {
                        a.chapters.get(i).map(|c| {
                            (
                                c.id.clone(),
                                a.id.clone(),
                                a.title.clone(),
                                chapter_label(c),
                            )
                        })
                    })
                else {
                    return;
                };
                let now = self.favs.toggle(favorites::Fav::chapter(
                    cid.clone(),
                    label,
                    album_id,
                    album_title,
                ));
                self.save_favs();
                if self.photo.as_ref().map(|p| p.id.as_str()) == Some(cid.as_str()) {
                    self.reader_starred = now;
                }
                self.rebuild_album();
                self.emit();
            }
            _ => {}
        }
    }

    /// 收藏一变就落盘。失败不致命（内存里仍然是对的），只打日志。
    fn save_favs(&mut self) {
        if let Err(e) = self.favs.save() {
            log::warn!("保存收藏失败：{e}");
        }
    }

    // ------------------------------------------------------------ 本子

    /// 打开某个本子的详情页。
    ///
    /// ⚠️ 请求在**后台线程**上发（为什么见 [`Cmd::SearchDone`] 那段），
    ///    所以这里可以放心地「先给反馈、再等数据」。
    ///
    /// 关于那段老注释「必须先取数据再动界面」：异步化之前这条请求是在
    /// worker 上**同步**发的，那时确实必须先取数据 —— 否则模拟器里会把自己
    /// 卡死：fbshim 要把整帧（1236×1648 ≈ 2 MB）通过 virtio-serial 推给宿主，
    /// 而 QEMU 的 chardev 写入是阻塞式的，宿主读得慢就拖住 QEMU 主循环，
    /// 连带把 guest 的网络一起卡住，紧接着发的请求就「读响应首行超时」。
    ///
    /// 「渲染很贵」这条结论今天依然成立，但**已经掐不断这条请求了** ——
    /// 请求跑在别的线程上，UI 重绘卡的是 UI 线程。
    fn open_album(&mut self, album_id: &str) {
        let Some(base) = self.base.clone() else {
            self.say("还没连上服务端");
            self.emit();
            return;
        };

        let from = self.screen;
        self.album_req = Some((album_id.to_string(), from));

        // 立刻给反馈：详情页数据要几百毫秒到几秒，不给提示的话
        // 这一下点击看着就像没生效。
        self.loading = true;
        self.loading_text = "打开本子…".into();
        self.emit();

        let id = album_id.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let (found, tries) = match api::retry(3, || api::album(&base, &id)) {
                Ok((a, n)) => (Ok(a), n),
                Err(e) => (Err(e), 3),
            };
            let _ = tx.send(Cmd::AlbumDone {
                album_id: id,
                from,
                found,
                tries,
                ms: t0.elapsed().as_millis() as u64,
            });
        });
    }

    /// 本子详情回来了。
    fn apply_album(
        &mut self,
        album_id: String,
        from: Screen,
        found: Result<api::Album, api::ApiError>,
        tries: u32,
        ms: u64,
    ) {
        // ⚠️ 身份对不上就丢弃：用户可能已经点了别的本子，或者干脆走了。
        if self.album_req.as_ref() != Some(&(album_id.clone(), from)) || self.screen != from {
            return;
        }
        self.album_req = None;

        if tries > 1 {
            log::warn!("album 重试 {tries} 次才成功（模拟器网络抖动）");
        }
        let base = self.base.clone();
        match found {
            Ok(a) => {
                self.status = album_status(&a);
                self.album_title = a.title.clone();
                self.album_page = 0;
                // 记下「是从哪儿点进这个本子的」——目录页的「返回」要靠它。
                // ⚠️ 用的是**发请求时**记下的 `from`，不是现在的 `self.screen`：
                //    请求飞在路上的这几百毫秒里，用户可能已经翻到别处了，
                //    那时读 self.screen 会把「来路」记错，返回就去了错的地方。
                self.album_return = from;
                self.screen = Screen::Album;
                self.loading = false;

                // ⚠️ 必须先把封面清掉。
                //
                // 踩过的坑：打开新本子时不清，下面那次 emit 画出来的就是**上一本**
                // 的封面 —— 而新封面要等 load_album_cover 取回来才有。
                // 表现是「详情页显示的永远是之前看过的某个本子的封面」。
                self.album_cover = None;

                // 标签：排好版交给 UI，并留一份纯文字用于点击后搜索
                self.tag_texts = a.tags.clone();
                self.album_tags = layout_tags(&a.tags);

                // 先把界面摆出来（没有封面也能看），再去取图
                self.album = Some(a);
                self.rebuild_album();
                self.emit();

                if let Some(base) = base {
                    self.load_album_cover(&base);
                }
            }
            Err(e) => {
                self.loading = false;
                self.say(format!("打开本子失败（耗时 {ms}ms）：{e}"));
                self.emit();
            }
        }
    }

    /// 取详情页顶部那张封面。
    ///
    /// 单独一步、放在界面画完之后：封面是网络请求，慢的话（服务端要现下载）
    /// 不能让整个详情页干等着 —— 目录先显示出来，封面随后补上。
    ///
    /// ⚠️ 取到之后**必须 emit**。之前忘了这一步，新封面要等到下一次无关的重绘
    /// 才显示 —— 那时候用户可能已经在看别的本子了，于是永远看到错位的封面。
    fn load_album_cover(&mut self, base: &str) {
        let Some(url) = self.album.as_ref().map(|a| a.cover.clone()) else {
            return;
        };
        if url.is_empty() {
            return;
        }

        // 缓存里有就直接贴（再打开同一个本子时不用再走一趟网络）
        if let Some(cached) = self.covers.get(&url) {
            if let Some(t) = cached.clone() {
                self.album_cover = Some(t);
                self.emit();
            }
            return;
        }

        // 取图挪到后台线程（为什么见 [`Cmd::SearchDone`]）——
        // 详情页的目录已经显示出来了，不该为了这张图把 worker 占住。
        let base = base.to_string();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            // 和 load_covers 用同一个解码尺寸 —— 两处缓存键都是 URL，
            // 尺寸不一致的话会互相拿到对方的小图/大图。
            let thumb = api::retry(3, || api::fetch_bytes(&base, &url))
                .ok()
                .and_then(|(r, _)| r)
                .and_then(|b| render::decode_thumb(&b, THUMB_MAX, THUMB_MAX).ok());
            let _ = tx.send(Cmd::AlbumCoverDone { url, thumb });
        });
    }

    /// 详情页封面回来了。
    fn apply_album_cover(&mut self, url: String, thumb: Option<PixelBuffer>) {
        // 先入缓存 —— 缓存是按 URL 存的，跟「现在在哪个本子」无关，
        // 过期了也留着，下次打开同一个本子能直接命中。
        self.covers.insert(url.clone(), thumb.clone());

        // ⚠️ 取回的过程中用户可能已经打开别的本子了。不是这个本子的封面就丢弃，
        //    否则会重现那个老 bug：「详情页显示的永远是之前看过的某个本子的封面」。
        if self.album.as_ref().map(|a| a.cover.as_str()) != Some(url.as_str()) {
            log::info!("[封面] {url} 取回时已经不在这个本子了，丢弃");
            return;
        }

        if let Some(t) = thumb {
            self.album_cover = Some(t);
            // 必须 emit —— 封面是异步取回来的，不推一次 UI 它就永远不会显示。
            // （apply_view 里封面是无条件设置的，所以光 emit 就够，不用动版本号）
            self.emit();
        }
        // 取不到就留空 —— 详情页那个位置本来就是灰底占位框
    }

    fn rebuild_album(&mut self) {
        // 先把要显示的名字拷出来，避免「借用 self.album」和「改 self 其它字段」撞车
        let labels: Vec<String> = match self.album.as_ref() {
            Some(a) => a.chapters.iter().map(chapter_label).collect(),
            None => return,
        };

        let total = labels.len();
        let pages = total.div_ceil(CHAPTERS_PER_PAGE).max(1);
        if self.album_page >= pages {
            self.album_page = pages - 1;
        }
        let start = self.album_page * CHAPTERS_PER_PAGE;
        let end = (start + CHAPTERS_PER_PAGE).min(total);

        // Slint 的 for 是一维的，两列网格在这里拆成左右两个模型
        let mut left = Vec::new();
        let mut right = Vec::new();
        for i in start..end {
            // 章节也能收藏（阅读器菜单「收藏本话」存的同一份数据），
            // 已收藏的显示实心星，空心星点一下就能收藏
            let cid = self
                .album
                .as_ref()
                .and_then(|a| a.chapters.get(i))
                .map(|c| c.id.clone())
                .unwrap_or_default();
            let row = RowData {
                label: labels[i].clone(),
                sub: String::new(),
                sub_label: String::new(),
                idx: i as i32,
                cover: None,
                star: chapter_star(&self.favs, &cid),
            };
            if (i - start) % 2 == 0 {
                left.push(row);
            } else {
                right.push(row);
            }
        }
        self.col_left = left;
        self.col_right = right;

        // ⚠️ **这里绝对不能碰 `self.rows`。**
        //
        // 原来有一句 `self.rows.clear()`，把搜索结果列表**整个清空**了。
        // `rows` 是搜索页、历史页共用的一份数据，而目录用的是 col_left/col_right ——
        // 目录看不见 rows，却把它清掉了，于是：
        //
        //     换排序 → 点进任一本子 → 返回 → 搜索列表**整片空白**
        //
        // 目录页自己一行都读不到 rows（`if screen == 4` 那段只读 col-left/col-right），
        // 所以留着完全无害。
        //
        // 同理下面三个字段也是共用的，但它们是**按屏渲染**的，目录改掉之后
        // 搜索页就再也不刷新它们了 —— 所以返回搜索页时必须补算一次，
        // 见 `restore_pager()`。
        let (p, n, info) = pager(self.album_page, pages);
        self.has_prev = p;
        self.has_next = n;
        self.list_info = info;
        self.rows_seq += 1;
    }

    /// 重新算出「当前这一屏」该显示的翻页状态。
    ///
    /// 为什么需要：`has_prev` / `has_next` / `list_info` 是四个屏共用的，
    /// 而目录页每次重建都会把它们改成目录自己的值。从目录退回搜索页/历史页时，
    /// 如果不补算一次，底栏就会显示**上一个屏**的页码 —— 比如「2/5」还亮着
    /// 「下一页」，而搜索结果其实只有 1 页。
    fn restore_pager(&mut self) {
        match self.screen {
            Screen::Search => {
                // search_page 是 1 起的，pager 要 0 起的
                let i = (self.search_page.max(1) - 1) as usize;
                let (p, n, info) = pager(i, self.search_page_count as usize);
                self.has_prev = p;
                self.has_next = n;
                self.list_info = info;
            }
            Screen::History => {
                let pages = self.history.len().div_ceil(HISTORY_PER_PAGE).max(1);
                let (p, n, info) = pager(self.history_page, pages);
                self.has_prev = p;
                self.has_next = n;
                self.list_info = info;
            }
            Screen::Favorites => {
                let pages = self.fav_items.len().div_ceil(HISTORY_PER_PAGE).max(1);
                let (p, n, info) = pager(self.fav_page, pages);
                self.has_prev = p;
                self.has_next = n;
                self.list_info = info;
            }
            Screen::Album => {
                let chapters = self.album.as_ref().map(|a| a.chapters.len()).unwrap_or(0);
                let pages = chapters.div_ceil(CHAPTERS_PER_PAGE).max(1);
                let (p, n, info) = pager(self.album_page, pages);
                self.has_prev = p;
                self.has_next = n;
                self.list_info = info;
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ 章节与阅读

    fn open_chapter(&mut self, index: usize) {
        let Some(base) = self.base.clone() else {
            return;
        };
        // 一次性把要用的字段拷出来，借用立刻结束
        let (photo_id, album_id, album_title, label) = match self
            .album
            .as_ref()
            .and_then(|a| a.chapters.get(index).map(|c| (a, c)))
        {
            Some((a, c)) => (
                c.id.clone(),
                a.id.clone(),
                a.title.clone(),
                chapter_label(c),
            ),
            None => return,
        };

        self.photo_req = Some(photo_id.clone());
        self.loading = true;
        self.loading_text = "载入章节…".into();
        self.emit();

        let tx = self.tx.clone();
        let pid = photo_id.clone();
        std::thread::spawn(move || {
            let found = api::retry(3, || api::photo(&base, &pid)).map(|(p, _)| p);
            if found.is_ok() {
                // 顺手记一笔观看记录：打开本子时服务端已记过一次，这里在
                // **开始读**的时候再记一次，让「最近观看」跟着实际阅读走。
                //
                // ⚠️ 这也是个网络请求，同样不能留在 worker 上；失败不影响阅读。
                let _ = api::post_history(&base, &album_id, &album_title);
            }
            let _ = tx.send(Cmd::PhotoDone {
                photo_id: pid,
                album_title,
                label,
                found,
            });
        });
    }

    /// 章节详情回来了 → 进阅读器。
    fn apply_photo(
        &mut self,
        photo_id: String,
        album_title: String,
        label: String,
        found: Result<api::Photo, api::ApiError>,
    ) {
        // 身份对不上就丢弃：用户可能已经点了别的一话。
        if self.photo_req.as_ref() != Some(&photo_id) {
            return;
        }
        self.photo_req = None;

        match found {
            Ok(p) => {
                self.album_title = album_title;
                self.photo = Some(p);
                self.page = 0;
                self.raw_cache.clear();

                self.enter_reader();
                self.loading = true;
                self.loading_text = "载入第 1 页…".into();
                self.load_page(0);
            }
            Err(e) => {
                // ⚠️ 这里必须把 loading 关掉：上面刚把它打开，不关的话
                //    「载入中」那一条会一直挂在屏幕上不走。
                self.loading = false;
                self.say(format!("载入「{label}」失败：{e}"));
                self.emit();
            }
        }
    }

    fn page_total(&self) -> usize {
        if let Some(local) = &self.local {
            return local.len();
        }
        self.photo
            .as_ref()
            .map(|p| p.page_count as usize)
            .unwrap_or(0)
    }

    /// 这一页要切成几段（0/1 = 不用解密）。抽成自由函数，方便用测试钉住。
    fn seg_for_page(&self, page: usize) -> u32 {
        self.photo.as_ref().map(|p| page_seg(p, page)).unwrap_or(0)
    }

    /// 当前章节的标识，用作缓存键。
    fn chapter_key(&self) -> String {
        if self.local.is_some() {
            return "local".into();
        }
        self.photo.as_ref().map(|p| p.id.clone()).unwrap_or_default()
    }

    /// 取某页的原始字节 —— **只服务本地模式**（`--demo` / `--dir`）。
    ///
    /// ⚠️ **这里刻意没有网络分支，别加回来。**
    ///
    /// 网络模式的取图（几百 KB 传输 + 解码）整个跑在后台线程上，见
    /// [`fetch_and_decode_page`] 和 [`Cmd::PageReady`]。这个函数是
    /// **同步**的，一旦在这里发起网络请求，就等于把 worker 又堵回去 ——
    /// 那正是这一轮要修的毛病。签名里留个 `local` 的 `expect`，
    /// 万一将来有人从网络路径调进来，会**立刻炸**而不是悄悄卡住。
    fn page_bytes(&mut self, page: usize) -> Result<Option<Vec<u8>>, String> {
        let key = (self.chapter_key(), page);
        if let Some(b) = self.raw_cache.get(&key) {
            return Ok(Some(b.clone()));
        }

        let local = self
            .local
            .as_ref()
            .expect("page_bytes 只服务本地模式；网络取图请走 fetch_and_decode_page");
        let bytes = match local.get(page) {
            Some(LocalPage::Demo(n)) => Ok(Some(demo_ppm(*n))),
            Some(LocalPage::File(p)) => std::fs::read(p)
                .map(Some)
                .map_err(|e| format!("读 {} 失败: {e}", p.display())),
            None => Ok(None),
        };

        if let Ok(Some(b)) = &bytes {
            if self.raw_cache.len() >= RAW_CACHE_MAX {
                self.raw_cache.clear();
            }
            self.raw_cache.insert(key, b.clone());
        }
        bytes
    }

    fn load_page(&mut self, page: usize) {
        let total = self.page_total();
        if page >= total {
            self.say("已经是最后一页");
            self.emit();
            return;
        }

        // 本地模式（--demo / --dir）读的是本地文件或现生成的测试图，
        // **不走网络** —— 保持原来的同步路径：没必要多开线程，
        // 而且模拟器那几套自检脚本（ime-test / reader-menu-test）行为完全不变。
        if self.local.is_some() {
            self.load_page_local(page);
            return;
        }

        let Some(base) = self.base.clone() else {
            return;
        };
        let Some((url, jpeg_url)) = self
            .photo
            .as_ref()
            .and_then(|p| p.images.get(page))
            .map(|img| (img.url.clone(), img.jpeg_url.clone()))
        else {
            self.say("这一页服务端取不到");
            self.emit();
            return;
        };
        // 本页的分割数（0/1 = 不用解密）。见 descramble.rs 的字段约定。
        let seg = self.seg_for_page(page);

        // 身份 = (章节, 页码, 适屏方式)。三者任一变了，这张图就不能用了。
        let key = self.chapter_key();
        let fit = self.fit_mode;
        self.page_req = Some((key.clone(), page, fit));

        // 立刻给反馈：一页要传几百 KB，真机上好几秒。
        self.loading = true;
        self.loading_text = format!("载入第 {} 页…", page + 1);
        self.emit();

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = fetch_and_decode_page(&base, &url, &jpeg_url, fit, seg);
            let _ = tx.send(Cmd::PageReady {
                key,
                page,
                fit,
                result,
            });
        });
    }

    /// 本地模式的一页：读取 + 解码，同步做完。
    fn load_page_local(&mut self, page: usize) {
        let bytes = match self.page_bytes(page) {
            Ok(Some(b)) => b,
            Ok(None) => {
                self.say("这一页服务端取不到");
                self.emit();
                return;
            }
            Err(e) => {
                self.say(format!("取图失败：{e}"));
                self.emit();
                return;
            }
        };
        match decode_page(bytes, self.fit_mode) {
            Ok(d) => self.apply_page_image(page, d),
            Err(e) => {
                self.loading = false;
                self.say(e);
                self.emit();
            }
        }
    }

    /// 一页解好了 → 上屏。
    fn apply_page_image(&mut self, page: usize, d: DecodedPage) {
        // 进缓存：翻回来 / 预取时就不用再下载了
        if self.raw_cache.len() >= RAW_CACHE_MAX {
            self.raw_cache.clear();
        }
        self.raw_cache.insert((self.chapter_key(), page), d.raw);

        self.page = page;
        self.strip = d.strip;
        self.strip_step = d.strip_step;
        self.strip_y = d.strip_y;
        self.display = Some(d.display);
        self.scroll_mode = d.long;

        self.loading = false;
        self.update_reader_status();
        self.image_seq += 1;
        self.emit();

        // 预取下一页：翻页时就不用干等网络了（预取本身也在后台线程上）
        self.prefetch(page + 1);
    }

    /// 后台线程解好的一页回来了。
    fn apply_page_ready(
        &mut self,
        key: String,
        page: usize,
        fit: render::FitMode,
        result: Result<DecodedPage, String>,
    ) {
        // ⚠️ **身份对不上就丢弃。** 图片来源不一样（换页 / 切适屏 / 换章节 /
        //    已经退出阅读器），拿旧图去上屏会出现「翻到第 3 页却显示第 2 页」，
        //    或者退出阅读器后画面又被一张漫画盖住。
        if self.page_req.as_ref() != Some(&(key, page, fit)) {
            return;
        }
        self.page_req = None;

        match result {
            Ok(d) => self.apply_page_image(page, d),
            Err(e) => {
                self.loading = false;
                self.say(e);
                self.emit();
            }
        }
    }

    /// 把下一页的字节先拉进缓存。失败无所谓，翻到那儿再取就是了。
    ///
    /// ⚠️ 同样得在**后台线程**上做：它也是几百 KB 的下载，留在 worker 上
    ///    等于每次翻完页又堵一次（原来就是这么堵的）。
    fn prefetch(&mut self, page: usize) {
        if self.local.is_some() || page >= self.page_total() {
            return;
        }
        let Some(base) = self.base.clone() else {
            return;
        };
        let Some(url) = self
            .photo
            .as_ref()
            .and_then(|p| p.images.get(page))
            .map(|i| i.url.clone())
        else {
            return;
        };
        let key = (self.chapter_key(), page);
        if self.raw_cache.contains_key(&key) {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Ok(Some(b)) = api::fetch_bytes(&base, &url) {
                let _ = tx.send(Cmd::Prefetched { key, bytes: b });
            }
        });
    }

    fn reader_title(&self) -> String {
        if self.local.is_some() {
            return "本地测试图".into();
        }
        if let Some(p) = self.photo.as_ref() {
            if !p.title.is_empty() {
                return p.title.clone();
            }
        }
        if !self.album_title.is_empty() {
            return self.album_title.clone();
        }
        "阅读中".into()
    }

    /// 进阅读器时的统一动作：切屏、算本话收藏状态、决定要不要弹一次性说明。
    ///
    /// ⚠️ 弹说明的条件是「标记文件不存在」，不是「本次启动没弹过」——
    /// 规格 4.9 要求点过「知道了」之后**永不再现**，所以得落盘。
    fn enter_reader(&mut self) {
        self.screen = Screen::Reader;
        self.reader_menu = false;
        self.reader_starred = self
            .photo
            .as_ref()
            .map(|p| self.favs.contains_chapter(&p.id))
            .unwrap_or(false);
        self.reader_hint = !hint_seen();
    }

    /// 阅读器菜单「收藏本话」：收藏 / 取消当前这一话。
    fn toggle_current_chapter_fav(&mut self) {
        let (Some(p), Some(a)) = (self.photo.as_ref(), self.album.as_ref()) else {
            // 本地单章模式没有 photo/album，这一项不生效
            return;
        };
        // 本子标题一并存下（收藏页单话行要显示它）——
        // `a` 正是当前这一话所属的本子，直接拿它的标题。
        let fav = favorites::Fav::chapter(
            p.id.clone(),
            self.chapter_name(),
            a.id.clone(),
            a.title.clone(),
        );
        let now = self.favs.toggle(fav);
        self.reader_starred = now;
        if let Err(e) = self.favs.save() {
            log::warn!("保存收藏失败：{e}");
        }
        log::info!(
            "{}本话「{}」",
            if now { "收藏" } else { "取消收藏" },
            self.chapter_name()
        );
    }

    fn update_reader_status(&mut self) {
        let title = self.reader_title();
        let (cur, total) = self.reader_progress();

        if self.scroll_mode {
            self.status = format!("{title}  ·  第 {cur}/{total} 屏");
            self.list_info = format!("{cur}/{total}");
        } else {
            self.status = format!("{title}  ·  第 {cur}/{total} 页");
            self.list_info = format!("{cur}/{total}");
        }
        // 换了一话，收藏状态跟着这一话走
        self.reader_starred = self
            .photo
            .as_ref()
            .map(|p| self.favs.contains_chapter(&p.id))
            .unwrap_or(false);
    }

    /// 长条漫是不是已经滚到底了。
    fn at_strip_bottom(&self) -> bool {
        match self.strip.as_ref() {
            Some(s) => self.strip_y + self.strip_step >= s.height() as usize,
            None => true,
        }
    }

    /// 翻页 / 滚动。左键传 -1，右键传 +1。
    ///
    /// 长条漫会先在自己这一页里滚一屏，滚到头再自动变成翻页 ——
    /// 这样一页长图不用点十几次才能过去。
    fn turn(&mut self, delta: i32) {
        if self.screen != Screen::Reader {
            return;
        }
        // 菜单开着的时候翻页手势被拦截层吃掉了，走不到这里。
        // 真走到这儿（比如菜单刚被关掉那一下）也顺手把菜单收起来，避免状态残留。
        if self.reader_menu {
            self.reader_menu = false;
        }
        if self.scroll_mode {
            let at_edge = if delta > 0 {
                self.at_strip_bottom()
            } else {
                self.strip_y == 0
            };
            if !at_edge {
                self.scroll(delta);
                return;
            }
        }
        self.turn_page(delta);
    }

    fn scroll(&mut self, delta: i32) {
        let (h, step) = match self.strip.as_ref() {
            Some(s) => (s.height() as usize, self.strip_step),
            None => return,
        };
        if step == 0 {
            return;
        }

        let max_y = h.saturating_sub(step);
        let want = self.strip_y as i32 + delta * step as i32;
        let y = want.clamp(0, max_y as i32) as usize;

        if y == self.strip_y {
            // 已经贴边了，再点就翻页
            self.turn_page(delta);
            return;
        }

        // 先算出画布再改自己的字段：canvas 是新建的，和 strip 没有借用关系
        let (canvas, real_y) = {
            let strip = self.strip.as_ref().expect("上面刚确认过有");
            let (c, ry, _) = render::slice_to_canvas(strip, y);
            (c, ry)
        };
        self.strip_y = real_y;
        self.display = Some(canvas);
        self.update_reader_status();
        self.image_seq += 1;
        self.emit();
    }

    /// 只翻页，不管长条漫的滚动。
    fn turn_page(&mut self, delta: i32) {
        let total = self.page_total();
        let next = self.page as i32 + delta;

        if next < 0 {
            self.say("已经是第一页");
            self.emit();
            return;
        }
        if next as usize >= total {
            // 本章读完，试着接下一章，读起来是连贯的
            if delta > 0 && self.open_next_chapter() {
                return;
            }
            self.say("已经是最后一页");
            self.emit();
            return;
        }

        self.load_page(next as usize);
    }

    /// 本章读完了，接着打开下一章。返回 true 表示已经切过去了。
    fn open_next_chapter(&mut self) -> bool {
        let pos = match (self.album.as_ref(), self.photo.as_ref()) {
            (Some(a), Some(p)) => a.chapters.iter().position(|c| c.id == p.id),
            _ => None,
        };
        let Some(pos) = pos else { return false };

        let has_next = self
            .album
            .as_ref()
            .map(|a| pos + 1 < a.chapters.len())
            .unwrap_or(false);
        if !has_next {
            return false;
        }

        self.open_chapter(pos + 1);
        true
    }

    // ------------------------------------------------------------ 列表翻页

    fn list_page(&mut self, delta: i32) {
        match self.screen {
            Screen::Search => {
                let p = self.search_page as i32 + delta;
                if p >= 1 && p <= self.search_page_count as i32 {
                    self.do_search(p as u32);
                }
            }
            Screen::History => {
                let p = self.history_page as i32 + delta;
                if p >= 0 {
                    self.history_page = p as usize;
                    self.rebuild_history();
                    self.emit();
                }
            }
            Screen::Favorites => {
                let pages = self.fav_items.len().div_ceil(HISTORY_PER_PAGE).max(1);
                let p = self.fav_page as i32 + delta;
                if p >= 0 && (p as usize) < pages {
                    self.fav_page = p as usize;
                    self.rebuild_favorites();
                    self.emit();
                }
            }
            Screen::Album => {
                let chapters = self.album.as_ref().map(|a| a.chapters.len()).unwrap_or(0);
                let pages = chapters.div_ceil(CHAPTERS_PER_PAGE).max(1);
                let p = self.album_page as i32 + delta;
                if p >= 0 && (p as usize) < pages {
                    self.album_page = p as usize;
                    self.rebuild_album();
                    self.emit();
                }
            }
            _ => {}
        }
    }

    fn activate(&mut self, idx: i32) {
        if idx < 0 {
            return;
        }
        let idx = idx as usize;

        match self.screen {
            Screen::Search => {
                if let Some(id) = self.search_ids.get(idx).cloned() {
                    self.open_album(&id);
                }
            }
            Screen::History => {
                if let Some(id) = self.history.get(idx).map(|h| h.id.clone()) {
                    self.open_album(&id);
                }
            }
            Screen::Favorites => {
                if let Some(fav) = self.fav_items.get(idx).cloned() {
                    self.open_fav(&fav);
                }
            }
            Screen::Album => self.open_chapter(idx),
            _ => {}
        }
    }

    // ------------------------------------------------------------ 返回

    fn back(&mut self) {
        let from = self.screen;
        let target = back_target(
            from,
            self.nav_return,
            self.album_return,
            self.album.is_some(),
            self.input_purpose,
        );

        // 离开阅读器的收尾：菜单要收掉，跟阅读器相关的画面状态全清干净
        if from == Screen::Reader {
            self.reader_menu = false;
            self.reader_hint = false;
            self.display = None;
            self.scroll_mode = false;
            self.strip = None;
            self.image_seq += 1;
        }

        match from {
            // 回目录：得把目录重新铺出来
            Screen::Reader if target == Screen::Album => {
                self.rebuild_album();
                self.status = self
                    .album
                    .as_ref()
                    .map(album_status)
                    .unwrap_or_default();
            }
            Screen::Reader => self.say("本地测试模式没有目录"),
            Screen::Search if self.input_purpose == InputPurpose::Search => self.say("选择要做什么"),
            _ => {}
        }

        self.screen = target;
        // 落回列表屏时按**这一屏自己的**数据重算翻页状态。
        // 目录页重建时会把 has_prev/has_next/list_info 改成目录的页码，
        // 不退回来重算的话，底栏显示的就是上一个屏的页码。
        self.restore_pager();
        self.emit();
    }
}

/// 底栏该高亮第几项。−1 = 都不亮（详情页这类子页面 / 快速入门 / 阅读器 / 出错页）。
///
/// 设置有两屏（第 1 页和关于页），都算高亮第 4 项 —— 用户眼里它们是同一个地方。
fn nav_active(screen: Screen) -> i32 {
    match screen {
        Screen::History => 0,
        Screen::Search => 1,
        Screen::Home => 2,
        Screen::Favorites => 3,
        Screen::Settings | Screen::SettingsAbout => 4,
        _ => -1,
    }
}

/// 去掉地址里的 `http://`。界面上显示 `IP:端口` 更清爽（规格里就是那么写的）。
fn strip_scheme(base: &str) -> &str {
    base.strip_prefix("http://").unwrap_or(base)
}

/// 列表翻页状态：`(有上一页, 有下一页, 页码文本)`。
///
/// `index` 从 **0** 起（搜索页存的是 1 起的 `search_page`，用之前要减 1）。
///
/// 抽成纯函数，是因为**三个列表屏共用同一组 `has_prev` / `has_next` /
/// `list_info`**。以前每处各算各的，很容易漏掉一处 —— 表现就是底栏显示的
/// 是上一个屏的页码，或者「下一页」该灭没灭。
fn pager(index: usize, pages: usize) -> (bool, bool, String) {
    let pages = pages.max(1);
    let index = index.min(pages - 1);
    (index > 0, index + 1 < pages, format!("{}/{}", index + 1, pages))
}

/// 当前该给刷新策略设多大的间隔。
///
/// **只有阅读器才开**（返回 0 = 策略从不主动全刷）。抽成纯函数是为了能写测试 ——
/// 这道门一旦漏了，表现是「首页、设置页也在闪」，很容易被当成正常的。
fn refresh_interval_for(screen: Screen, full_refresh: u32) -> u32 {
    if screen == Screen::Reader {
        full_refresh
    } else {
        0
    }
}

/// 「返回」该去哪一屏。
///
/// 抽成**纯函数**是为了能用单元测试钉住 —— 这条逻辑最容易错，
/// 而错了的表现又很隐蔽（点得动、但落到错屏上，看着像「没反应」）。
///
/// ⚠️ 关键是**每个屏用不同的返回变量**，两者不能混用：
///
/// | 从哪儿按返回 | 回到哪 | 变量 |
/// |---|---|---|
/// | 输入页 / 设置 / 收藏 | 谁把它打开的 | `nav_return` |
/// | 本子目录 | 从哪儿点进这个本子的 | `album_return` |
///
/// `purpose` 只对搜索页有意义：它既是底栏的「搜索」标签，又是唯一的输入面。
/// 当输入面用时它回到 `nav_return`，当标签用时就回首屏。
///
/// 踩过的坑：本子目录原来用的是 `nav_return`。而阅读器菜单里的「刷新频率」
/// 也要开输入页，那会把 `nav_return` 覆盖成 `Reader` ——
/// 于是「阅读器 → 菜单 → 刷新频率 → 目录 → 返回」就弹回漫画页，
/// 来回死循环，只能退出程序。
fn back_target(
    screen: Screen,
    nav_return: Screen,
    album_return: Screen,
    has_album: bool,
    purpose: InputPurpose,
) -> Screen {
    match screen {
        // 从阅读器退回本子目录。本地单章模式没有目录，回**首屏**（历史记录）。
        Screen::Reader => {
            if has_album {
                Screen::Album
            } else {
                Screen::History
            }
        }
        Screen::Album => album_return,
        // 搜索页兼作输入面：当标签页用时回首屏；当输入面用时回谁把它打开的
        Screen::Search => {
            if purpose == InputPurpose::Search {
                Screen::History
            } else {
                nav_return
            }
        }
        // 收藏 / 设置都是底栏打开的二级页，回它们各自的来源
        Screen::Favorites | Screen::Settings | Screen::SettingsAbout => nav_return,
        Screen::Guide => Screen::Home,
        Screen::Home => Screen::History,
        Screen::History | Screen::Error => Screen::History,
    }
}

// ---------------------------------------------------------------- UI 应用

thread_local! {
    /// UI 线程上次应用过的内容版本号 (image_seq, rows_seq, kb_seq)。
    /// 电子墨水整屏刷一次要几百毫秒，没变的东西就别碰。
    static APPLIED: std::cell::RefCell<(u64, u64, u64)> =
        std::cell::RefCell::new((u64::MAX, u64::MAX, u64::MAX));
}

fn to_model(rows: &[RowData]) -> slint::ModelRc<ListRow> {
    let v: Vec<ListRow> = rows
        .iter()
        .map(|r| {
            // 框的尺寸也在这里算 —— 只有这里同时拿得到图和显示上限。
            //
            // ⚠️ 上限用 `GRID_COVER`（340）而不是列表行那个 168×232：
            //    `cover-w/h` 现在只有搜索页的三列网格读（`CoverCell`），
            //    而那一格是 376 宽、封面最多 340 —— 按 168 算的话，
            //    方正的大封面会缩成一小张、居中孤零零挂着；按 360 算又会
            //    顶到下面第一行标题（y=376）。
            //    列表行（`ListLine`）根本不读这两个值，它自己写死 72×96 + contain，
            //    所以换上限对它们**没有任何影响**。
            let (bw, bh) = cover_box(r.cover.as_ref(), GRID_COVER, GRID_COVER);
            ListRow {
                label: r.label.clone().into(),
                sub: r.sub.clone().into(),
                sub_label: r.sub_label.clone().into(),
                cover: match r.cover.as_ref() {
                    Some(b) => slint::Image::from_rgba8(b.clone()),
                    None => slint::Image::default(),
                },
                idx: r.idx,
                cover_w: bw as i32,
                cover_h: bh as i32,
                star: r.star,
            }
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(v))
}

fn apply_view(app: &AppWindow, v: &View) {
    app.set_screen(v.screen as i32);
    app.set_conn_text(v.conn_text.clone().into());
    app.set_nav_active(v.nav_active);
    app.set_list_info(v.list_info.clone().into());
    app.set_has_prev(v.has_prev);
    app.set_has_next(v.has_next);
    app.set_list_sub(v.list_sub.clone().into());
    app.set_loading(v.loading);
    app.set_loading_text(v.loading_text.clone().into());
    app.set_loaded_count(v.loaded_count);
    app.set_total_count(v.total_count);
    app.set_scroll_mode(v.scroll_mode);

    // ---- 搜索 / 输入 ----
    app.set_input_text(v.input_text.clone().into());
    app.set_recent_searches(to_string_model(&v.recent));
    app.set_show_recent(v.show_recent);
    app.set_kb_shown(v.kb_shown);
    // Slint 的 length 到 Rust 这边就是 f32
    app.set_row_h(v.row_h as f32);
    app.set_cand_row_a(to_string_model(&v.cand_row_a));
    app.set_cand_sel(v.cand_sel);
    app.set_cand_fs(v.cand_fs);
    app.set_cand_note(v.cand_note.clone().into());
    app.set_cand_empty(v.cand_empty);
    app.set_cand_has_prev(v.cand_has_prev);
    app.set_cand_has_next(v.cand_has_next);

    // 搜索排序（服务端没报 sort_options 时是空表，界面自动不显示这一行）
    app.set_sort_labels(to_string_model(&v.sort_labels));
    app.set_sort_active(v.sort_active);

    // ---- 设置 / 关于 ----
    app.set_set_addr(v.set_addr.clone().into());
    app.set_set_conn(v.set_conn.clone().into());
    app.set_refresh_label(v.refresh_label.clone().into());
    app.set_refresh_available(v.refresh_available);
    app.set_cache_label(v.cache_label.clone().into());
    app.set_version_label(v.version_label.clone().into());
    app.set_device_label(v.device_label.clone().into());
    app.set_server_label(v.server_label.clone().into());
    app.set_pinyin_label(v.pinyin_label.clone().into());
    app.set_author_label(v.author_label.clone().into());

    // ---- 收藏 / 快速入门 ----
    app.set_fav_tab(v.fav_tab);
    app.set_fav_right(v.fav_right.clone().into());
    app.set_guide_page(v.guide_page);

    // ---- 数字小键盘覆盖层 ----
    app.set_keypad_shown(v.keypad_shown);
    app.set_keypad_title(v.keypad_title.clone().into());
    app.set_keypad_value(v.keypad_value.clone().into());
    app.set_keypad_len(v.keypad_len);
    app.set_keypad_note(v.keypad_note.clone().into());

    // ---- 出错页 ----
    app.set_err_title(v.err_title.clone().into());
    app.set_err_desc(v.err_desc.clone().into());
    app.set_err_addr(v.err_addr.clone().into());
    app.set_err_state(v.err_state.clone().into());
    app.set_err_last(v.err_last.clone().into());

    // ---- 阅读器 ----
    app.set_reader_title(v.reader_title.clone().into());
    app.set_fit_label(v.fit_label.clone().into());
    app.set_reader_menu(v.reader_menu);
    app.set_reader_can_fit(v.reader_can_fit);
    app.set_reader_pos(v.reader_pos.clone().into());
    app.set_reader_chapters(v.reader_chapters.clone().into());
    app.set_reader_starred(v.reader_starred);
    app.set_reader_hint(v.reader_hint);

    // ---- 详情页 ----
    app.set_album_title(v.album_title.clone().into());
    app.set_album_byline(v.album_byline.clone().into());
    // ⚠️ 封面**无条件**设置，包括为 None 的时候也要设（设成空图）。
    //
    // 踩过的坑：原来写成 `if let Some(buf) = ...`，于是打开新本子、
    // album_cover 被清成 None 时，UI 上那张**上一本的封面根本没被覆盖掉**，
    // 一直挂在那儿 —— 表现就是「详情页显示的永远是之前看过的某个本子的封面」。
    app.set_album_cover(match v.album_cover.as_ref() {
        Some(buf) => slint::Image::from_rgba8(buf.clone()),
        None => slint::Image::default(),
    });
    // 框的尺寸跟着图走。**同样无条件设置**（理由同上）——
    // 清空封面时要退回最大框占位，不能留着上一本的比例。
    app.set_album_cover_w(v.album_cover_w);
    app.set_album_cover_h(v.album_cover_h);

    APPLIED.with(|cell| {
        let mut applied = cell.borrow_mut();

        if v.rows_seq != applied.1 {
            applied.1 = v.rows_seq;
            app.set_rows(to_model(&v.rows));
            app.set_col_left(to_model(&v.col_left));
            app.set_col_right(to_model(&v.col_right));
            app.set_album_tags(to_tag_model(&v.album_tags));
        }

        if v.kb_seq != applied.2 {
            applied.2 = v.kb_seq;
            // 键盘三行只在「字母页 / 符号页」切换时才换内容
            app.set_kb_row_1(to_string_model(&v.kb_rows[0]));
            app.set_kb_row_2(to_string_model(&v.kb_rows[1]));
            app.set_kb_row_3(to_string_model(&v.kb_rows[2]));
        }

        if v.image_seq != applied.0 {
            applied.0 = v.image_seq;
            // 图像在这里才组装成 slint::Image —— 它不是 Send，
            // 所以只能在 UI 线程上从 Send 的像素缓冲拼出来。
            //
            // ⚠️ **无条件设置，包括为 None 的时候**（设成空图）。
            // 踩过的坑和封面那个一模一样：原来写成 `if let Some(...)`，
            // 于是 page_image 被清成 None 时，UI 上那张**旧图根本没被覆盖**，
            // 一直挂在那儿。详情页封面就是这么显示错本子的。
            // 目前没暴露是因为离开阅读器后目标屏都是全屏白底、把旧图盖住了 ——
            // 属于「哪天布局一改就会冒出来」的那类。
            app.set_page_image(match v.page_image.as_ref() {
                Some(buf) => slint::Image::from_rgba8(buf.clone()),
                None => slint::Image::default(),
            });
        }
    });
}

// ---------------------------------------------------------------- 测试图

/// 生成一张测试图（PPM），给 `--demo` 用。
///
/// PPM 未压缩、不需要编码器，`image` crate 的 `pnm` 特性直接能解 ——
/// 这条路是为了让模拟器自检完全不依赖网络和磁盘素材。
fn demo_ppm(page: usize) -> Vec<u8> {
    const W: usize = 1236;
    const H: usize = 1648;
    let mut buf = format!("P6\n{W} {H}\n255\n").into_bytes();
    buf.reserve(W * H * 3);

    let digits: Vec<usize> = page
        .to_string()
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(|d| d as usize)
        .collect();
    let scale = 24usize;
    let glyph_w = 5 * scale;
    let glyph_h = 7 * scale;
    let total_w = glyph_w * digits.len().max(1);
    let start_x = W.saturating_sub(total_w) / 2;
    let start_y = H.saturating_sub(glyph_h) / 2;

    for y in 0..H {
        for x in 0..W {
            // 外边框 + 对角线：对角线用来一眼确认缩放没有变形
            let border = x < 6 || y < 6 || x >= W - 6 || y >= H - 6;
            let diag = (x as isize - (y as isize * W as isize / H as isize)).abs() < 3;
            let mut ink = border || diag;

            if !ink && y >= start_y && y < start_y + glyph_h && x >= start_x {
                let gx = (x - start_x) / glyph_w;
                let px = ((x - start_x) % glyph_w) / scale;
                let py = (y - start_y) / scale;
                if gx < digits.len()
                    && px < 5
                    && py < 7
                    && FONT_5X7[digits[gx]][py] & (1 << (4 - px)) != 0
                {
                    ink = true;
                }
            }

            let v = if ink { 0u8 } else { 255u8 };
            buf.push(v);
            buf.push(v);
            buf.push(v);
        }
    }
    buf
}

/// 5x7 点阵数字 0-9，每行低 5 位有效。
const FONT_5X7: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111], // 2
    [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110], // 5
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100], // 9
];

/// 章节在目录里的显示名。
///
/// jmcomic 的 `name` 常是「第 1 话」，`indextitle` 是更完整的一行，两个都可能为空，
/// 所以逐级退：indextitle → name → 「第 N 话」。
fn chapter_label(c: &api::Chapter) -> String {
    let t = if !c.indextitle.is_empty() {
        &c.indextitle
    } else {
        &c.title
    };
    let t = t.trim();
    if t.is_empty() {
        format!("第 {} 话", c.index)
    } else {
        t.to_string()
    }
}

/// 本子标题那一行：标题 · 作者 · 话数。
fn album_status(a: &api::Album) -> String {
    let mut s = a.title.clone();
    if !a.author.is_empty() {
        s.push_str("  ");
        s.push_str(&a.author);
    }
    if a.chapter_count > 1 {
        s.push_str(&format!("  ·  {} 话", a.chapter_count));
    }
    s.push_str(&format!("  ·  共 {} 页", a.page_count));
    s
}

/// 标签区的排版参数。**这些值和 ui/app.slint 里的坐标是配套的**，改一处要改两处。
mod tag_layout {
    // ⚠️ 这几个是**相对于标签区左上角的偏移**，不是屏幕绝对坐标。
    //
    // 原来它们是绝对坐标（X0=300 / Y0=252），因为那时标签挤在封面**右边**。
    // v5 详情页改成「封面在上、标签在其下通栏」，于是 300/252 这种值
    // 会让标签整个跑到右下角去 —— 表现就是「标签压在目录上」。
    // 现在统一成偏移量，区域原点由 app.slint 那边给（60px 页边距 + y=508）。
    /// 标签区左边界（区内偏移）
    pub const X0: i32 = 0;
    /// 标签区右边界：1236 − 60×2 = 1116
    pub const X1: i32 = 1116;
    /// 标签区上边界（区内偏移）
    pub const Y0: i32 = 0;
    /// 一个标签的高度。**必须和 app.slint 里画的一致** ——
    /// 这里原来是 52、界面画 66，两边不一致，两行标签就多算 28px，
    /// 正好压到目录第一行上。
    pub const H: i32 = 66;
    pub const GAP_X: i32 = 12;
    pub const GAP_Y: i32 = 14;
    /// 最多铺几行 —— 多了会挤到下面的目录区
    pub const MAX_ROWS: i32 = 2;
}

/// 估算一个标签占多少像素宽。
///
/// 只是排版用的近似值，差几个像素看不出来。中日文按一个字 26px 估，
/// 其余（拉丁字母/数字/空格）按 14px。两边各留 16px 内边距。
fn chip_width(s: &str) -> i32 {
    let mut text = 0;
    for c in s.chars() {
        text += if (c as u32) > 0x2E80 { 26 } else { 14 };
    }
    (text + 32).clamp(72, tag_layout::X1 - tag_layout::X0)
}

/// 把标签排成流式布局：一行放不下就换行。
///
/// 为什么在 Rust 里算：Slint 没有内置的换行流式布局，而标签的数量和长度
/// 都是运行时才知道的。算好 x/y/宽 传过去，UI 那边只按坐标画，简单可靠。
///
/// 超过 `MAX_ROWS` 行就**截断**而不是继续往下排 —— 目录区在那底下，
/// 标签把目录挤没了得不偿失。
fn layout_tags(tags: &[String]) -> Vec<TagData> {
    use tag_layout::*;

    let max_w = X1 - X0;
    let mut out = Vec::new();
    let mut x = 0i32;
    let mut row = 0i32;

    for (i, t) in tags.iter().enumerate() {
        if row >= MAX_ROWS {
            break;
        }
        let w = chip_width(t);

        // 这一行放不下 → 换行（第一个不换，否则窄标签也会被挤下去）
        if x > 0 && x + w > max_w {
            row += 1;
            x = 0;
            if row >= MAX_ROWS {
                break;
            }
        }

        out.push(TagData {
            label: t.clone(),
            x: X0 + x,
            y: Y0 + row * (H + GAP_Y),
            w,
            idx: i as i32,
        });
        x += w + GAP_X;
    }

    out
}

/// 测试用：造一条**没人接收**的命令管道。
///
/// `Worker` 手里有一条发送端 —— 后台线程取完封面用它把结果送回来
/// （见 [`Cmd::CoverReady`]）。测试里不会真的联网取图，随便给一条即可；
/// 用 `mem::forget` 把接收端留住，免得发送端一 `send` 就报 `Err`，
/// 让将来可能写的测试误以为「送回去了」。
#[cfg(test)]
fn test_tx() -> Sender<Cmd> {
    let (tx, rx) = channel::<Cmd>();
    std::mem::forget(rx);
    tx
}

/// 解密字段的取值优先级（服务端逐页 `seg` → `filename`+`scramble_id` → 5 位页号）。
#[cfg(test)]
mod seg_tests {
    use super::*;

    fn 造章节(id: &str, sid: Option<u64>, seg: Option<u32>, filename: &str, pages: usize) -> api::Photo {
        api::Photo {
            id: id.into(),
            title: "测试".into(),
            indextitle: String::new(),
            index: "1".into(),
            album_id: String::new(),
            page_count: pages as u32,
            page_base: 0,
            scramble_id: sid,
            images: (0..pages)
                .map(|_| api::ImageRef {
                    page: 0,
                    url: "/img/x/0".into(),
                    jpeg_url: String::new(),
                    seg,
                    filename: filename.into(),
                })
                .collect(),
        }
    }

    /// 服务端直接给了 seg —— 要用它，不再自己算（哪怕和 md5 结果不同）。
    #[test]
    fn 优先用服务端给的_seg() {
        let p = 造章节("1475681", Some(220980), Some(4), "", 3);
        assert_eq!(page_seg(&p, 0), 4);
        // 明确给 0 = 服务端说这页不加密
        let p0 = 造章节("1475681", Some(220980), Some(0), "", 3);
        assert_eq!(page_seg(&p0, 0), 0);
    }

    /// 只给 scramble_id：按 5 位页号算（实测源站都是 00001 这种命名）。
    #[test]
    fn 只给_scramble_id_时按页号算() {
        let p = 造章节("1475681", Some(220980), None, "", 3);
        assert_eq!(page_seg(&p, 0), 10, "第 1 页");
        assert_eq!(page_seg(&p, 1), 8, "第 2 页");
    }

    /// 服务端给了原始文件名就优先用它。
    #[test]
    fn 给了文件名就用文件名() {
        let p = 造章节("1475681", Some(220980), None, "00019", 1);
        assert_eq!(page_seg(&p, 0), 14, "00019 的分段数应为 14");
    }

    /// 旧版 Python 服务端不返回 scramble_id —— 图已经解好密了，不能再切。
    #[test]
    fn 没有_scramble_id_不解密() {
        let p = 造章节("1475681", None, None, "", 3);
        assert_eq!(page_seg(&p, 0), 0);
        assert_eq!(page_seg(&p, 2), 0);
    }

    /// 章节 ID 小于 scramble_id 的本子本来就没加密。
    #[test]
    fn 小号本子不解密() {
        let p = 造章节("5888", Some(220980), None, "", 1);
        assert_eq!(page_seg(&p, 0), 0);
    }

    /// 越界页不能 panic。
    #[test]
    fn 越界页安全() {
        let p = 造章节("1475681", Some(220980), None, "", 1);
        assert_eq!(page_seg(&p, 9), 0);
    }

    /// **解密必须真的作用到上屏画布** —— 光算对分段数不够。
    ///
    /// 造一条「从上到下越来越亮」的渐变，按 descramble 的逆映射打乱，
    /// 再走 `finish_decode`。解密对了 → 画布上暗下亮；不解密 → 下暗上亮。
    #[test]
    fn 解密后的画布方向正确() {
        const W: u32 = 100;
        const H: u32 = 100;
        const NUM: u32 = 5;

        let mut src = PixelBuffer::new(W, H);
        for (y, row) in src.make_mut_bytes().chunks_exact_mut(W as usize * 4).enumerate() {
            let v = (y * 255 / (H as usize - 1)) as u8;
            for px in row.chunks_exact_mut(4) {
                px[0] = v;
                px[1] = v;
                px[2] = v;
                px[3] = 255;
            }
        }

        // 打乱（descramble 的逆运算）
        let piece = (H / NUM) as usize;
        let over = (H % NUM) as usize;
        let mut scrambled = PixelBuffer::new(W, H);
        {
            let s = src.as_bytes();
            let d = scrambled.make_mut_bytes();
            let rb = W as usize * 4;
            for i in 0..NUM as usize {
                let dst_start0 = piece * i + if i == 0 { 0 } else { over };
                let len = piece + if i == 0 { over } else { 0 };
                let src_start0 = H as usize - piece * (i + 1) - over;
                let so = dst_start0 * rb;
                let dof = src_start0 * rb;
                d[dof..dof + len * rb].copy_from_slice(&s[so..so + len * rb]);
            }
        }

        let avg = |page: &DecodedPage, y0: usize, y1: usize| -> f64 {
            let bytes = page.display.as_bytes();
            let w = page.display.width() as usize;
            let sum: u64 = (y0..y1)
                .flat_map(|y| {
                    (0..w).map(move |x| {
                        let i = (y * w + x) * 4;
                        (77 * bytes[i] as u32 + 150 * bytes[i + 1] as u32 + 29 * bytes[i + 2] as u32)
                            >> 8
                    })
                })
                .map(|v| v as u64)
                .sum();
            sum as f64 / ((y1 - y0) * w) as f64
        };

        let fixed = finish_decode(
            scrambled.clone(),
            Vec::new(),
            render::FitMode::FitHeight,
            NUM,
            H,
        );
        let loose = finish_decode(
            scrambled,
            Vec::new(),
            render::FitMode::FitHeight,
            0,
            H,
        );

        let h = fixed.display.height() as usize;
        let top = avg(&fixed, 0, 100);
        let bottom = avg(&fixed, h - 100, h);
        assert!(top < bottom, "解密后该「上暗下亮」，实际 top={top:.1} bottom={bottom:.1}");

        let top0 = avg(&loose, 0, 100);
        assert!(
            top0 > top + 50.0,
            "不解密时顶部应该明显更亮（说明 seg 真的生效了）：{top0:.1} vs {top:.1}"
        );
    }
}

/// 异步请求的**过期结果必须被丢弃**。
///
/// 这是异步化最容易踩的坑：请求发出去之后用户随时可能改主意（换关键词、
/// 点别的本子、翻到别的页、退出阅读器）。回来时若不加校验，轻则
/// 「上一屏的数据贴到这一屏」，重则把用户**从当前页面硬拽走**。
///
/// 本质上和 README 第 32 条「多个屏共用一个字段」是同一类错误，
/// 只是从「同步互相踩」变成了「异步越过界」。
#[cfg(test)]
mod async_guard_tests {
    use super::*;

    fn 新_w() -> Worker {
        Worker::new(
            Default::default(),
            None,
            RefreshHandle::default(),
            1,
            "测试".into(),
            test_tx(),
        )
    }

    /// 搜索结果回来时用户已经改了关键词 —— 这份结果不能再上屏。
    #[test]
    fn 搜索过期结果被丢弃() {
        let mut w = 新_w();
        w.screen = Screen::Search;
        w.loading = true;
        w.search_req = Some(("naruto".into(), "mr".into(), 1, Screen::Search));

        // 回来的却是「sakura」的结果
        w.apply_search(
            "sakura".into(),
            "mr".into(),
            1,
            Err(api::ApiError::Net("测试".into())),
        );

        assert!(w.loading, "过期结果不该顺手把 loading 关掉");
        assert!(w.search_req.is_some(), "过期结果不该清掉在飞请求的记录");
        assert_eq!(w.screen, Screen::Search);
    }

    /// 身份对得上时正常收下（对照组，免得上面的测试变成「永远丢弃」也通过）。
    #[test]
    fn 搜索对得上的结果被收下() {
        let mut w = 新_w();
        w.screen = Screen::Search;
        w.loading = true;
        w.search_req = Some(("naruto".into(), "mr".into(), 1, Screen::Search));

        w.apply_search(
            "naruto".into(),
            "mr".into(),
            1,
            Err(api::ApiError::Net("测试".into())),
        );

        assert!(!w.loading, "对得上的结果该处理掉（这里是失败分支，要关 loading）");
        assert!(w.search_req.is_none(), "处理完就该清掉在飞记录");
    }

    /// ⭐ 用户在搜索加载期间点了底栏走人 —— 结果回来**不能把他拽回搜索页**。
    ///
    /// 这是这一轮异步化最典型的坑：请求飞在路上时用户随时可以走。
    /// 不管的话，`apply_search` 里那句 `self.screen = Screen::Search`
    /// 会把人硬拽回去；而且它还会覆盖 `rows`（搜索/历史共用），
    /// 让历史页显示出搜索结果 —— README 第 32 条那类坑的异步版。
    #[test]
    fn 搜索加载期间走开则结果被丢弃() {
        let mut w = 新_w();
        w.screen = Screen::Search;
        w.loading = true;
        w.search_req = Some(("naruto".into(), "mr".into(), 1, Screen::Search));

        // 用户在结果回来之前点了底栏「历史」
        w.screen = Screen::History;

        w.apply_search(
            "naruto".into(),
            "mr".into(),
            1,
            Err(api::ApiError::Net("测试".into())),
        );

        assert_eq!(w.screen, Screen::History, "不该把用户从历史页拽回搜索页");
        assert!(w.search_req.is_some(), "这一份已作废，但记录留着由下一次覆盖");
    }

    /// 标签搜索是从**详情页**发起的，靠结果回来才切到搜索页 ——
    /// 这条路径不能被「还在不在搜索页」的守卫误伤。
    #[test]
    fn 详情页发起的标签搜索仍然能上屏() {
        let mut w = 新_w();
        w.screen = Screen::Album; // 从详情页点标签
        w.loading = true;
        w.search_req = Some(("原神".into(), "mr".into(), 1, Screen::Album));

        w.apply_search(
            "原神".into(),
            "mr".into(),
            1,
            Err(api::ApiError::Net("测试".into())),
        );

        // 身份和屏都对得上 → 收下（这里走失败分支，所以 loading 关掉）
        assert!(!w.loading);
        assert!(w.search_req.is_none());
    }

    /// 用户已经离开历史页 —— 数据晚回来时**不能把他拽回去**。
    #[test]
    fn 离开历史页后结果被丢弃() {
        let mut w = 新_w();
        w.screen = Screen::Reader; // 用户已经进阅读器了
        w.apply_history(Ok(vec![api::HistoryItem {
            id: "1".into(),
            title: "某本子".into(),
        }]));

        assert_eq!(w.screen, Screen::Reader, "不该把用户从阅读器拽回历史页");
        assert!(w.history.is_empty(), "不该往历史列表里塞东西");
    }

    /// 用户已经点了别的本子 —— 前一个的结果不能贴上去。
    #[test]
    fn 本子详情过期结果被丢弃() {
        let mut w = 新_w();
        w.screen = Screen::Search;
        w.album_req = Some(("111".into(), Screen::Search));
        w.apply_album(
            "222".into(),
            Screen::Search,
            Err(api::ApiError::Net("测试".into())),
            1,
            5,
        );

        assert!(w.album.is_none(), "不该把别的本子的详情贴上屏");
        assert!(w.album_req.is_some(), "过期结果不该清掉在飞记录");
    }

    /// 点进本子之后趁加载时走人 —— 详情回来不能把人拽回详情页。
    #[test]
    fn 本子加载期间走开则结果被丢弃() {
        let mut w = 新_w();
        w.screen = Screen::History; // 用户已经点了底栏走人
        w.album_req = Some(("111".into(), Screen::Search)); // 是从搜索页点进去的

        w.apply_album(
            "111".into(),
            Screen::Search,
            Err(api::ApiError::Net("测试".into())),
            1,
            5,
        );

        assert_eq!(w.screen, Screen::History, "不该把用户拽进详情页");
        assert!(w.album.is_none());
    }

    /// 翻页的图回来晚了 —— 不能在已经翻到别的页时把旧图上屏。
    /// 这是最显眼的一种错位：「翻到第 3 页却显示第 2 页」。
    #[test]
    fn 整页图过期结果被丢弃() {
        let mut w = 新_w();
        w.page = 5;
        w.page_req = Some(("ch1".into(), 5, render::FitMode::FitHeight));

        let 旧图 = DecodedPage {
            raw: Vec::new(),
            strip: None,
            strip_step: 0,
            strip_y: 0,
            display: PixelBuffer::new(1, 1),
            long: false,
        };
        // 用户已经翻到第 6 页了，第 5 页的图才回来
        w.apply_page_ready("ch1".into(), 5, render::FitMode::FitHeight, Ok(旧图));

        // 注意：这里 page_req 是 (ch1,5,FitHeight)，回来的也正是它 ——
        // 所以这一份是「对得上」的，会被收下；下面再试一份真的过期的。
        assert_eq!(w.page, 5);

        // 现在模拟「用户翻到第 6 页后再回来第 5 页的图」
        w.page = 6;
        w.page_req = Some(("ch1".into(), 6, render::FitMode::FitHeight));
        let 更旧的图 = DecodedPage {
            raw: Vec::new(),
            strip: None,
            strip_step: 0,
            strip_y: 0,
            display: PixelBuffer::new(1, 1),
            long: false,
        };
        w.apply_page_ready("ch1".into(), 5, render::FitMode::FitHeight, Ok(更旧的图));
        assert_eq!(w.page, 6, "过期的整页图不该上屏");
    }

    /// 用户连点了两次「重试」—— 第一轮的结果晚回来时不能覆盖第二轮。
    ///
    /// 这条特别要紧：第二轮可能已经连上了新地址，而第一轮的结果里
    /// 是旧地址（或者干脆是「没找到」），覆盖上去会把用户打回出错页。
    #[test]
    fn 过期的连接结果被丢弃() {
        let mut w = 新_w();
        w.boot_seq = 5; // 用户已经点了第 5 次重试
        w.status = "第五轮".into();

        // 第 3 轮的结果现在才回来
        w.apply_boot_status(3, "正在连接（试 3 个候选地址）…".into());
        assert_eq!(w.status, "第五轮", "过期轮次的进度不该刷屏");

        w.apply_boot(3, None, true, true);
        assert_ne!(
            w.screen,
            Screen::Error,
            "过期轮次的「没找到」不该把用户打进错误页"
        );
        // 过期轮次的结论也不该写回（否则下一次会错误地跳过候选地址）
        assert!(!w.skip_saved, "过期轮次不该改 skip_saved");
    }

    /// 最新一轮的结果正常收下（对照组）。
    #[test]
    fn 当前轮次的连接结果被收下() {
        let mut w = 新_w();
        w.boot_seq = 5;
        w.apply_boot_status(5, "正在局域网里找服务端…".into());
        assert_eq!(w.status, "正在局域网里找服务端…");

        // 一个地址都没连上 → 该出错页
        w.apply_boot(5, None, true, true);
        assert_eq!(w.screen, Screen::Error);
        assert!(w.skip_saved && w.skip_env, "结论要写回，下次直接走广播");
    }

    /// 切了适屏方式之后，按旧方式裁好的图也不能用了。
    #[test]
    fn 适屏方式变了之后旧图被丢弃() {
        let mut w = 新_w();
        w.page = 3;
        w.page_req = Some(("ch1".into(), 3, render::FitMode::FitWidth));

        let 图 = DecodedPage {
            raw: Vec::new(),
            strip: None,
            strip_step: 0,
            strip_y: 0,
            display: PixelBuffer::new(1, 1),
            long: false,
        };
        // 回来的是按「竖边适应」裁的
        w.apply_page_ready("ch1".into(), 3, render::FitMode::FitHeight, Ok(图));

        assert!(w.page_req.is_some(), "适屏方式对不上，该丢弃并保留在飞记录");
    }
}

#[cfg(test)]
mod tag_tests {
    use super::*;
    use tag_layout::*;

    /// 标签必须排进标签区范围内，不能溢出去压到封面或目录。
    #[test]
    fn 标签不越界() {
        let tags: Vec<String> = (0..30).map(|i| format!("标签{i}")).collect();
        for t in layout_tags(&tags) {
            assert!(
                t.x >= X0 && t.x + t.w <= X1,
                "标签「{}」x={} w={} 超出了 {X0}..{X1}",
                t.label, t.x, t.w
            );
            assert!(t.y >= Y0, "标签「{}」y={} 跑到上面去了", t.label, t.y);
            // 最多 MAX_ROWS 行
            let rows = (t.y - Y0) / (H + GAP_Y);
            assert!(rows < MAX_ROWS, "标签「{}」排到了第 {rows} 行，超了", t.label);
        }
    }

    /// 行数超限时要截断，而不是一直往下排（下面还有目录区）。
    #[test]
    fn 标签超行截断() {
        let tags: Vec<String> = (0..100).map(|i| format!("很长的标签名字{i}")).collect();
        let out = layout_tags(&tags);
        assert!(out.len() < 100, "应该截断，实际排了 {} 个", out.len());
        assert!(!out.is_empty(), "不该一个都不排");
    }

    /// 空列表不该 panic，也不该产生任何标签。
    #[test]
    fn 空标签安全() {
        assert!(layout_tags(&[]).is_empty());
    }

    /// 单个超长的标签要被夹到标签区宽度以内，不能溢出。
    #[test]
    fn 超长标签被夹住() {
        let long = "这是一个特别特别特别特别特别特别特别特别特别特别特别长的标签名字".to_string();
        let out = layout_tags(&[long]);
        assert_eq!(out.len(), 1);
        assert!(
            out[0].x + out[0].w <= X1,
            "超长标签没被夹住：x={} w={}",
            out[0].x,
            out[0].w
        );
    }

    /// idx 要对应原始下标 —— 点错了标签就会搜错东西。
    #[test]
    fn 标签下标对应原数组() {
        let tags: Vec<String> = vec!["甲".into(), "乙".into(), "丙".into()];
        let out = layout_tags(&tags);
        assert_eq!(out.len(), 3);
        for (i, t) in out.iter().enumerate() {
            assert_eq!(t.idx, i as i32, "第 {i} 个标签的 idx 不对");
            assert_eq!(t.label, tags[i]);
        }
    }

    /// 同一行的标签不能重叠。
    #[test]
    fn 标签不重叠() {
        let tags: Vec<String> = (0..8).map(|i| format!("T{i}")).collect();
        let out = layout_tags(&tags);
        for a in &out {
            for b in &out {
                if a.idx >= b.idx || a.y != b.y {
                    continue; // 只看同一行、且 a 在 b 左边的情况
                }
                assert!(
                    a.x + a.w <= b.x,
                    "标签「{}」和「{}」重叠了",
                    a.label,
                    b.label
                );
            }
        }
    }
}

#[cfg(test)]
mod back_tests {
    use super::*;

    /// 全屏刷新**只在阅读器里开** —— 其它任何屏都不该闪。
    #[test]
    fn 只有阅读器会全刷() {
        for s in [
            Screen::History,
            Screen::Search,
            Screen::Home,
            Screen::Favorites,
            Screen::Settings,
            Screen::SettingsAbout,
            Screen::Guide,
            Screen::Album,
            Screen::Error,
        ] {
            assert_eq!(
                refresh_interval_for(s, 1),
                0,
                "{s:?} 不该开全屏刷新（用户反馈：首页和设置不用刷）"
            );
        }
        assert_eq!(
            refresh_interval_for(Screen::Reader, 1),
            1,
            "阅读器该按设置的全刷间隔走"
        );
        assert_eq!(refresh_interval_for(Screen::Reader, 3), 3);
        // 用户设成 0（关闭）时，阅读器里也不刷
        assert_eq!(refresh_interval_for(Screen::Reader, 0), 0);
    }

    /// 阅读器「返回」回本子目录（有本子时）。
    #[test]
    fn 阅读器返回目录() {
        assert_eq!(
            back_target(
                Screen::Reader,
                Screen::History,
                Screen::History,
                true,
                InputPurpose::Search
            ),
            Screen::Album
        );
    }

    /// 本地单章模式没有目录，阅读器返回回**首屏**（历史记录）。
    #[test]
    fn 无本子时阅读器回首屏() {
        assert_eq!(
            back_target(
                Screen::Reader,
                Screen::History,
                Screen::History,
                false,
                InputPurpose::Search
            ),
            Screen::History
        );
    }

    /// 设置和收藏用 `nav_return`（谁打开的它们）。
    #[test]
    fn 设置与收藏用_nav_return() {
        for s in [Screen::Settings, Screen::SettingsAbout, Screen::Favorites] {
            assert_eq!(
                back_target(s, Screen::History, Screen::Home, true, InputPurpose::Search),
                Screen::History,
                "{s:?} 应该回到 nav_return"
            );
        }
    }

    /// 搜索页身兼两职：当标签页用时回首屏；当输入面（填地址/数字）用时回打开它的屏。
    #[test]
    fn 搜索页看用途决定返回() {
        assert_eq!(
            back_target(
                Screen::Search,
                Screen::Reader,
                Screen::Home,
                true,
                InputPurpose::Search
            ),
            Screen::History,
            "搜索标签返回首屏"
        );
        assert_eq!(
            back_target(
                Screen::Search,
                Screen::Reader,
                Screen::Home,
                true,
                InputPurpose::Server
            ),
            Screen::Reader,
            "填地址时返回该回打开它的阅读器"
        );
    }

    /// 历史是首屏，按返回原地不动，不该跳走。
    #[test]
    fn 搜索与历史回首屏() {
        for s in [Screen::Search, Screen::History] {
            assert_eq!(
                back_target(s, Screen::History, Screen::History, true, InputPurpose::Search),
                Screen::History
            );
        }
    }

    /// **这条就是这个 bug 的回归测试。**
    ///
    /// 场景：阅读器 → 菜单 →「全屏刷新」（会开输入页，把 `nav_return` 写成
    /// `Reader`）→ 确定 → 菜单 →「章节列表」→ 目录页 → 按返回。
    ///
    /// 修复前这里用 `nav_return`，于是返回 `Reader` —— 弹回漫画页，
    /// 再从漫画页按返回又是目录页，来回死循环，只能退出程序。
    /// 修复后用的是 `album_return`，不受污染影响。
    #[test]
    fn 目录返回不受刷新频率污染() {
        // nav_return 被「全屏刷新」糟蹋成了 Reader
        let polluted_nav = Screen::Reader;
        // album_return 还是正确的：当初是从首屏点进这个本子的
        let album_return = Screen::History;

        assert_eq!(
            back_target(
                Screen::Album,
                polluted_nav,
                album_return,
                true,
                InputPurpose::Search
            ),
            Screen::History,
            "目录页返回被 nav_return 带偏了 —— 会弹回漫画页死循环"
        );
    }

    /// 从搜索结果点进本子时，目录返回该回搜索结果页。
    #[test]
    fn 目录返回跟着来源走() {
        assert_eq!(
            back_target(
                Screen::Album,
                Screen::Settings,
                Screen::Search,
                true,
                InputPurpose::Search
            ),
            Screen::Search
        );
        assert_eq!(
            back_target(
                Screen::Album,
                Screen::Reader,
                Screen::History,
                true,
                InputPurpose::Search
            ),
            Screen::History
        );
    }

    /// 一条完整走位：首屏 → 目录 → 阅读器 →（刷新频率污染 nav_return）→
    /// 目录 → 首屏。每一步都该落在预想的屏上，不该卡住。
    #[test]
    fn 走位一圈能回首屏() {
        let album_return = Screen::History; // 是从首屏点进这个本子的
        let has_album = true;
        let purpose = InputPurpose::Search;

        // 首屏 → 目录 → 阅读器（open_chapter 不改 nav_return，所以这里还是 History）
        let nav_return = Screen::Reader; // ← 阅读器里开输入页改刷新频率，被污染成 Reader

        // 前提校验：nav_return 确实被污染过。没有这句的话，将来 nav_return
        // 不再被污染时这个用例会**静默变成**一个什么都没测的绿灯。
        assert_eq!(nav_return, Screen::Reader, "这个用例的前提是 nav_return 被污染");

        // 阅读器 → 目录
        let at = back_target(Screen::Reader, nav_return, album_return, has_album, purpose);
        assert_eq!(at, Screen::Album, "阅读器该退回目录");

        // 目录 → 首屏  ← 修复前这里会弹回 Reader（死循环，只能退出程序）
        let at = back_target(at, nav_return, album_return, has_album, purpose);
        assert_eq!(at, Screen::History, "目录该退回首屏，而不是弹回漫画");

        // 首屏再按返回，原地不动，不该跳走
        assert_eq!(
            back_target(at, nav_return, album_return, has_album, purpose),
            Screen::History
        );
    }

    /// 底栏高亮：五个标签各自对上，设置两页都高亮「设置」。
    #[test]
    fn 底栏高亮对得上屏() {
        assert_eq!(nav_active(Screen::History), 0);
        assert_eq!(nav_active(Screen::Search), 1);
        assert_eq!(nav_active(Screen::Home), 2);
        assert_eq!(nav_active(Screen::Favorites), 3);
        assert_eq!(nav_active(Screen::Settings), 4);
        assert_eq!(nav_active(Screen::SettingsAbout), 4);
        for s in [Screen::Album, Screen::Reader, Screen::Guide, Screen::Error] {
            assert_eq!(nav_active(s), -1, "{s:?} 是子页面，底栏不该亮");
        }
    }

    /// 地址显示要去掉 `http://`（规格里写的是 `IP:端口`）。
    #[test]
    fn 地址去掉协议头() {
        assert_eq!(strip_scheme("http://192.168.1.7:8001"), "192.168.1.7:8001");
        assert_eq!(strip_scheme("192.168.1.7:8001"), "192.168.1.7:8001");
    }
}

#[cfg(test)]
mod pager_tests {
    use super::*;

    #[test]
    fn 单页时两边都灭() {
        assert_eq!(pager(0, 1), (false, false, "1/1".to_string()));
    }

    #[test]
    fn 首页只有下一页() {
        assert_eq!(pager(0, 3), (false, true, "1/3".to_string()));
    }

    #[test]
    fn 中间页两边都有() {
        assert_eq!(pager(1, 3), (true, true, "2/3".to_string()));
    }

    #[test]
    fn 末页只有上一页() {
        assert_eq!(pager(2, 3), (true, false, "3/3".to_string()));
    }

    /// 页数为 0（还没搜过、历史为空）当成 1 页，不能出现「0/0」这种页码。
    #[test]
    fn 零页当成一页() {
        assert_eq!(pager(0, 0), (false, false, "1/1".to_string()));
    }

    /// 下标越界要夹住 —— 否则会显示「10/3」，而且两个按钮都不该亮。
    #[test]
    fn 下标越界被夹住() {
        assert_eq!(pager(9, 3), (true, false, "3/3".to_string()));
    }
}

/// 「按确定到底搜什么」的规则。
///
/// 一句话：**只有「点候选」和「空格」把拼音变成中文，其余情况一律原样当英文。**
///
/// 这条以前是反的 —— 确定会无条件落第一个候选（`take()` 不管有没有选中过
/// 都返回 `cands[0]`），于是拼音模式下搜英文有两个相反方向的毛病：
/// 表里没有的音（`naruto`）什么都不上屏，恰好像是拼音的（`one`）被换成中文。
/// 详见 `Worker::raw_commit_text` 的注释。下面几条把两条路各自钉住。
#[cfg(test)]
mod ime_commit_tests {
    use super::*;

    /// 造一个「在搜索页键盘上敲了这串键」的状态。
    ///
    /// v5 没有模式键了：搜索页的字母一律进拼音缓冲，所以这里不用再设模式，
    /// 只把用途设成搜索（只有搜索会出候选）。
    fn 敲了(keys: &[&str]) -> Worker {
        let mut w = Worker::new(
            Default::default(),
            None,
            RefreshHandle::default(),
            1,
            "测试".into(),
            test_tx(),
        );
        w.input_purpose = InputPurpose::Search;
        w.screen = Screen::Search;
        for k in keys {
            w.type_key(k);
        }
        w
    }

    /// 搜索时打英文按确定 —— 搜的必须是那串英文本身。
    ///
    /// 修复前：`naruto` 在表里一个候选都没有，`take()` 返回 `None`，
    /// 输入框还是空的，按确定只弹一句「还没输入内容」——**英文根本搜不了**。
    #[test]
    fn 没挑候选就搜英文() {
        let mut w = 敲了(&["N", "A", "R", "U", "T", "O"]);
        assert!(w.compose.cands().is_empty(), "naruto 在表里不该有候选");
        assert_eq!(w.input, "", "没挑候选之前，已上屏的部分不该有东西");
        assert_eq!(w.input_text(), "naruto", "但输入框里要显示字母原文");

        w.key_confirm();
        assert_eq!(w.keyword, "naruto", "按确定该搜原样的英文");
    }

    /// 反过来的那半条：`one` 恰好是合法拼音，候选非空 ——
    /// 但用户**没点**，就不能擅自换成「哦呢」。
    #[test]
    fn 有候选但没挑也不改字() {
        let mut w = 敲了(&["O", "N", "E"]);
        assert!(!w.compose.cands().is_empty(), "one 是合法拼音，该有候选");

        w.key_confirm();
        assert_eq!(w.keyword, "one", "有候选 ≠ 用户挑了候选");
    }

    /// 挑过候选就变中文 —— 这是「只有选了才变中文」的正面。
    #[test]
    fn 点了候选就变中文() {
        let mut w = 敲了(&["O", "N", "E"]);
        w.commit_compose(); // 等价于点候选栏第一格
        assert_eq!(w.input, "哦呢");
        assert_eq!(w.input_text(), "哦呢", "上屏之后输入框就是汉字");

        w.key_confirm();
        assert_eq!(w.keyword, "哦呢");
    }

    /// 空格落高亮那个候选（手机输入法的习惯）—— 这条**故意没改**。
    #[test]
    fn 空格落选中的候选() {
        let mut w = 敲了(&["O", "N", "E"]);
        w.handle(Cmd::KeySpace);
        assert_eq!(w.input, "哦呢");
        assert!(!w.compose.composing(), "落定之后缓冲要清空");
    }

    /// 打到一半按数字键，拼音原样落下，不该被换成中文。
    ///
    /// 原来是落第一个候选，于是「beijing」+「1」会变成「北京1」；
    /// 按新规则得到「beijing1」—— 用户想搜的本就是这串字母。
    #[test]
    fn 按数字键时拼音原样落下() {
        let mut w = 敲了(&["B", "E", "I", "J", "I", "N", "G"]);
        assert!(!w.compose.cands().is_empty(), "beijing 该有候选（北京）");
        w.type_key("1");
        assert_eq!(w.input, "beijing1");
    }

    /// 切键盘页（字母 <-> 符号）不等于挑字：切走时没落定的拼音要原样留下。
    ///
    /// 这是原来「切中英模式」那条测试的继任者 —— 模式键没了，
    /// 但「切换键盘不等于挑候选」这条规则还在。
    #[test]
    fn 切符号页时拼音原样落下() {
        let mut w = 敲了(&["O", "N", "E"]);
        w.handle(Cmd::KeySymbols);
        assert!(w.kb_symbols, "该切到符号页");
        assert_eq!(w.input, "one");
    }

    /// **删除键**：已上屏的候选要**整段**撤掉、退回字母原文（规格 4.2 第 5 条），
    /// 不是逐个删汉字。
    #[test]
    fn 删除键整段撤回候选() {
        let mut w = 敲了(&["O", "N", "E"]);
        w.commit_compose();
        assert_eq!(w.input, "哦呢");

        w.handle(Cmd::KeyBackspace);
        assert_eq!(w.input, "", "候选整段撤掉了");
        assert_eq!(w.compose.buf(), "one", "字母原文退回了缓冲");
        assert_eq!(w.input_text(), "one", "输入框显示回字母");
    }

    /// 删除键先退字母，再退普通字符 —— 不能一上来就把整段东西删掉。
    #[test]
    fn 删除键先退拼音字母() {
        let mut w = 敲了(&["Y", "U", "A", "N"]);
        w.handle(Cmd::KeyBackspace);
        assert_eq!(w.compose.buf(), "yua", "先退掉最后一个字母");
        assert_eq!(w.input, "");
    }

    /// 非搜索用途（填地址/数字）不走拼音：打字母直接进输入框，不出候选。
    #[test]
    fn 非搜索用途不拼拼音() {
        let mut w = Worker::new(
            Default::default(),
            None,
            RefreshHandle::default(),
            1,
            "测试".into(),
            test_tx(),
        );
        w.input_purpose = InputPurpose::Server;
        for k in ["1", "9", "2", ".", "1", "6", "8"] {
            w.type_key(k);
        }
        assert_eq!(w.input, "192.168");
        assert!(!w.compose.composing(), "填地址不该进拼音缓冲");
        // 字母也一样直接进
        w.type_key("A");
        assert_eq!(w.input, "192.168a");
    }

    /// 输入框内容 = 已上屏汉字 + 还没确定的拼音原文，拼在一起。
    #[test]
    fn 输入框拼上未确定的拼音() {
        let mut w = 敲了(&["O", "N", "E"]);
        w.commit_compose(); // 上屏「哦呢」
        for k in ["S", "H", "E", "N"] {
            w.type_key(k);
        }
        assert_eq!(w.input, "哦呢");
        assert_eq!(w.input_text(), "哦呢shen");
    }

    /// 候选词栏：**横排一行 5 个**（v6），空态给说明文字，两端按能否翻页变灰。
    ///
    /// 高度恒定的保证不在这里 —— 那是界面的事（`cand-empty` 切换内容、
    /// `.candbar` 恒为 142px），由 `check-ui-spec.sh` 查。
    #[test]
    fn 候选区状态() {
        let w = 敲了(&["O", "N", "E"]);
        let (row, sel, fs, _note, empty, prev, next) = w.cand_panel();
        assert!(!empty, "有候选时不是空状态");
        assert_eq!(
            row.len(),
            ime::PAGE.min(w.compose.cands().len()),
            "一页该给 {} 个候选",
            ime::PAGE
        );
        assert!(sel >= 0);
        assert!(fs >= 26 && fs <= 44, "字号该在 26..44 之间，实际 {fs}");
        // 第一页永远不该能往前翻；能不能往后翻，取决于确实还有第二页。
        // （别写死「两侧都不能翻」—— `one` 会走分词回退，候选可能不止一页。）
        assert!(!prev, "第一页不该能往前翻");
        assert_eq!(next, w.compose.page_count() > 1, "能不能往后翻要跟总页数一致");

        // naruto 没有候选 -> 空状态 + 说明
        let w = 敲了(&["N", "A", "R", "U", "T", "O"]);
        let (row, sel, _fs, note, empty, prev, next) = w.cand_panel();
        assert!(empty, "没匹配时该走空状态");
        assert!(row.is_empty(), "空状态不该有候选格");
        assert_eq!(sel, -1);
        assert!(note.contains("原文"), "该提示可以搜字母原文：{note}");
        assert!(!prev && !next, "空状态没有候选可翻");
    }

    /// 候选格字号跟着**本页最长的那条**候选走。
    ///
    /// 钉的是「候选词被截成半截」：格子只有 204px，44px 的字最多放 4 个，
    /// 5 个字就得降到 36。设计稿 v6 只给了 44/36 两档，这里多加了两档 ——
    /// 那张表里有 6 个字以上的词，两档会撑破格子被 `elide` 截掉。
    #[test]
    fn 候选字号跟着最长的那条() {
        assert_eq!(cand_font_size(1), 44);
        assert_eq!(cand_font_size(4), 44, "4 个字 176px，204 的格子放得下");
        assert_eq!(cand_font_size(5), 36, "5 个字 44px 会撑破格子");
        assert_eq!(cand_font_size(6), 30);
        assert_eq!(cand_font_size(9), 26);
        // 不能为 0 或负数（空候选时调用方会给 1）
        assert!(cand_font_size(0) > 0);
    }

    /// 候选词长的时候，整行字号要跟着降 —— 不能只有那一格变小，
    /// 一行里字大小不一看着像排版坏了。
    #[test]
    fn 长候选时整行统一降字号() {
        // `eryanghualiu` = 二氧化硫（4 字）
        let w = 敲了(&["E", "R", "Y", "A", "N", "G", "H", "U", "A", "L", "I", "U"]);
        let (row, _, fs, _, empty, _, _) = w.cand_panel();
        assert!(!empty, "eryanghualiu 该有候选");
        let longest = row.iter().map(|s| s.chars().count()).max().unwrap();
        assert_eq!(fs, cand_font_size(longest), "字号该按最长的那条算");
    }

    /// 候选翻页的禁用态：到头那一侧的「<」「>」必须是灰的。
    ///
    /// 钉的是「按钮点了没反应」那种观感问题 —— 以前翻页只有 16px 高、
    /// 点了也不知道到底动没动。现在既给够点击高度，也在到头时明确变灰。
    #[test]
    fn 候选翻页禁用态() {
        let mut w = 敲了(&["Y", "I"]);
        assert!(w.compose.page_count() > 1, "yi 应该不止一页候选");

        let (_, _, _, _, _, prev, next) = w.cand_panel();
        assert!(!prev, "第一页不该能往前翻");
        assert!(next, "第一页该能往后翻");

        // 翻到最后一页，两侧反过来
        let last = w.compose.page_count() - 1;
        for _ in 0..last {
            w.compose.turn_page(1);
        }
        let (_, _, _, _, _, prev, next) = w.cand_panel();
        assert!(prev, "最后一页该能往前翻");
        assert!(!next, "最后一页不该能往后翻");
    }
}

// 原来这里有个 `caret_tests`，钉的是「按字符估宽度」那套（中日文 44 / ASCII 28
// + 字距 3）。那套已经整个删掉了 —— 估出来的值和实际渲染步进对不上，
// 误差随字数累积，是「打多了光标越跑越远」的根因。
//
// 现在光标由 `ui/app.slint` 的 `HorizontalLayout` 排在文字后面，
// 用的是渲染器自己的字体度量，**没有可估的参数，也就没有要钉的常量**。

/// 键盘布局的行数必须和 `ui/app.slint` 的键帽位置对得上（行宽 10 / 9 / 7）。
/// 行数错了不会报错，只会让键帽错位 —— 属于「不报错、静默失效」的那类。
#[cfg(test)]
mod kb_tests {
    use super::*;

    #[test]
    fn 字母页与符号页行宽一致() {
        for rows in [&KB_LETTERS, &KB_SYMBOLS] {
            assert_eq!(rows[0].len(), 10, "第一行该 10 个键");
            assert_eq!(rows[1].len(), 9, "第二行该 9 个键");
            assert_eq!(rows[2].len(), 7, "第三行该 7 个键（后面接「删除」功能键）");
        }
    }

    /// 符号页必须能打出 IP 地址要用的字符，否则没法填服务器地址。
    #[test]
    fn 符号页能打地址() {
        let all: Vec<&str> = KB_SYMBOLS.iter().flat_map(|r| r.iter().copied()).collect();
        for c in ["0", "1", ".", ":"] {
            assert!(all.contains(&c), "符号页缺了 {c:?}，地址填不了");
        }
    }
}

/// 数字小键盘覆盖层（全屏刷新 / 跳页 / 本子 ID）。
///
/// 这几条钉的都是「写错了不报错、只是跳错地方或静默失败」的边界：
/// 空输入、超范围、非数字、以及**覆盖层绝不能改 screen / nav_return**。
#[cfg(test)]
mod keypad_tests {
    use super::*;

    fn 新_w() -> Worker {
        Worker::new(
            Default::default(),
            None,
            RefreshHandle::default(),
            1,
            "测试".into(),
            test_tx(),
        )
    }

    /// 数字累积、退格、清除，以及空串退格不 panic、非数字被忽略。
    #[test]
    fn 输入累积退格清除() {
        let mut w = 新_w();
        w.open_keypad_overlay(InputPurpose::Page, "标题", String::new(), "说明");
        assert!(w.keypad_shown);

        for d in ["1", "2", "3"] {
            w.keypad_digit(d);
        }
        assert_eq!(w.keypad_value, "123");

        w.keypad_backspace();
        assert_eq!(w.keypad_value, "12");

        w.keypad_clear();
        assert_eq!(w.keypad_value, "");

        // 已经空了再退格，不能 panic
        w.keypad_backspace();
        assert_eq!(w.keypad_value, "");

        // 非数字 / 空串一律忽略
        w.keypad_digit("a");
        w.keypad_digit("");
        w.keypad_digit("12"); // 一次多个字符也不行（回调只给单个）
        assert_eq!(w.keypad_value, "");
    }

    /// 位数封顶：超了不再增长，防止数字串把输入框和光标撑爆。
    #[test]
    fn 位数封顶() {
        let mut w = 新_w();
        for _ in 0..KEYPAD_MAX_DIGITS + 5 {
            w.keypad_digit("9");
        }
        assert_eq!(w.keypad_value.chars().count(), KEYPAD_MAX_DIGITS);
    }

    /// `keypad_len` 跟着 `keypad_value` 一起算（Slint 字符串没有 .length）。
    #[test]
    fn 长度一起算() {
        let mut w = 新_w();
        w.open_keypad_overlay(InputPurpose::Page, "标题", String::new(), "说明");
        for d in ["1", "0", "7"] {
            w.keypad_digit(d);
        }
        assert_eq!(w.view().keypad_len, 3);
        w.keypad_backspace();
        assert_eq!(w.view().keypad_len, 2);
    }

    /// **覆盖层开 / 关都不能动 `screen`，也不能碰 `nav_return`。**
    /// 它是覆盖式的（规则 ⑨）—— 关掉之后用户必须还在原来那一屏。
    #[test]
    fn 覆盖层不动屏() {
        let mut w = 新_w();
        w.screen = Screen::Settings;
        w.nav_return = Screen::History;

        w.open_keypad_overlay(InputPurpose::Refresh, "t", "1".into(), "n");
        assert!(w.keypad_shown);
        assert_eq!(w.screen, Screen::Settings, "开覆盖层不该跳屏");
        assert_eq!(w.nav_return, Screen::History, "开覆盖层不该碰 nav_return");

        w.handle(Cmd::KeypadCancel);
        assert!(!w.keypad_shown);
        assert_eq!(w.screen, Screen::Settings, "取消后该留在设置页");
        assert_eq!(w.nav_return, Screen::History, "取消不该碰 nav_return");
    }

    /// 刷新间隔的分派：空 = 回默认；越界夹到上限；非数字给反馈。
    #[test]
    fn 刷新间隔分派() {
        assert_eq!(
            keypad_action(InputPurpose::Refresh, "", 0),
            KeypadAction::FullRefresh(FULL_REFRESH_DEFAULT),
            "空输入 = 回默认"
        );
        assert_eq!(
            keypad_action(InputPurpose::Refresh, "5", 0),
            KeypadAction::FullRefresh(5)
        );
        assert_eq!(
            keypad_action(InputPurpose::Refresh, "0", 0),
            KeypadAction::FullRefresh(0),
            "0 = 关闭"
        );
        assert_eq!(
            keypad_action(InputPurpose::Refresh, "12345", 0),
            KeypadAction::FullRefresh(FULL_REFRESH_MAX),
            "超上限要夹住"
        );
        assert!(matches!(
            keypad_action(InputPurpose::Refresh, "1x", 0),
            KeypadAction::Invalid(_)
        ));
    }

    /// 页码夹到 `1..=总页数`；空 / 没页时直接关、不报错。
    #[test]
    fn 页码夹取() {
        assert_eq!(
            keypad_action(InputPurpose::Page, "8", 24),
            KeypadAction::GotoPage(8)
        );
        assert_eq!(
            keypad_action(InputPurpose::Page, "0", 24),
            KeypadAction::GotoPage(1),
            "0 贴到第 1 页"
        );
        assert_eq!(
            keypad_action(InputPurpose::Page, "999", 24),
            KeypadAction::GotoPage(24),
            "超了就贴到最后一页"
        );
        assert_eq!(
            keypad_action(InputPurpose::Page, "", 24),
            KeypadAction::Close,
            "空输入直接关，不报错"
        );
        assert_eq!(
            keypad_action(InputPurpose::Page, "3", 0),
            KeypadAction::Close,
            "没有页时关掉"
        );
    }

    /// 本子 ID：纯数字才打开；空直接关；非数字给反馈。
    #[test]
    fn 本子id校验() {
        assert_eq!(
            keypad_action(InputPurpose::AlbumId, "1478602", 0),
            KeypadAction::OpenAlbum("1478602".into())
        );
        assert_eq!(
            keypad_action(InputPurpose::AlbumId, "", 0),
            KeypadAction::Close,
            "空 ID 直接关，不报错"
        );
        assert!(
            matches!(
                keypad_action(InputPurpose::AlbumId, "12a", 0),
                KeypadAction::Invalid(_)
            ),
            "含非数字要给反馈"
        );
    }

    /// 非法输入**不关**覆盖层，并把反馈写进 note —— 不能静默失败。
    #[test]
    fn 非法输入不关覆盖层() {
        let mut w = 新_w();
        w.open_keypad_overlay(InputPurpose::Refresh, "t", String::new(), "原始说明");
        w.keypad_value = "12x".into(); // 绕过数字键，直接塞非法内容
        w.keypad_ok();
        assert!(w.keypad_shown, "非法输入不该关掉覆盖层");
        assert_ne!(w.keypad_note, "原始说明", "该把反馈写进 note");
    }

    /// 合法输入的确定会关掉覆盖层。
    #[test]
    fn 合法输入关覆盖层() {
        let mut w = 新_w();
        w.open_keypad_overlay(InputPurpose::Page, "t", String::new(), "n");
        w.keypad_value = "5".into();
        // page_total 为 0（还没打开章节），按 Page 分派会 Close
        w.keypad_ok();
        assert!(!w.keypad_shown);
    }
}

fn to_tag_model(tags: &[TagData]) -> slint::ModelRc<TagChip> {
    let v: Vec<TagChip> = tags
        .iter()
        .map(|t| TagChip {
            label: t.label.clone().into(),
            x: t.x,
            y: t.y,
            w: t.w,
            idx: t.idx,
        })
        .collect();
    slint::ModelRc::new(slint::VecModel::from(v))
}

/// 字符串数组 → Slint 的字符串模型。候选栏、键盘三行、排序按钮都用它。
///
/// 键盘每一行是一个独立的模型：行宽（10 / 9 / 7）和 `ui/app.slint` 里
/// 键帽的 `i * 112px` 是配套的，所以不做「按取模算列号」那种花活。
fn to_string_model(items: &[String]) -> slint::ModelRc<slint::SharedString> {
    let v: Vec<slint::SharedString> = items.iter().map(|s| s.as_str().into()).collect();
    slint::ModelRc::new(slint::VecModel::from(v))
}

// ---------------------------------------------------------------- main

/// 网络层自检：**不创建窗口**，只按顺序打一遍接口并打印耗时。
///
/// 存在的意义是「把变量一个一个摘掉」。UI 出问题时，可能的嫌疑人有好几个：
/// 服务端、网络（slirp/真机网卡）、ureq、Slint 渲染、线程模型。这个模式把
/// Slint 和 UI 线程整个拿掉，只留网络，于是能立刻二分出问题在哪一半。
///
/// 配合 `qemu-arm-static` 还能再摘掉一层：直接在 WSL 里跑 ARM 二进制，
/// 网络走 WSL 自己的协议栈，于是「ARM 代码」和「QEMU 虚拟网络」也分开了。
fn run_diag() {
    // ---- 先把「地址是从哪来的」这件事摊开 ----
    //
    // 真机上最常见的困惑是「服务端明明在跑，客户端却说连不上」。
    // 把每个来源单独列出来、各自探一次活，一眼就能看出是哪个环节的问题：
    // 是地址填错了，还是那个来源压根没提供地址，还是地址对但网络不通。
    eprintln!(
        "[diag] 程序目录   : {}",
        config_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "无法确定".into())
    );
    match saved_base() {
        Some(b) => eprintln!("[diag] 用户手填   : {b}"),
        None => eprintln!("[diag] 用户手填   : (无 server.txt)"),
    }
    match env_base() {
        Some(b) => eprintln!("[diag] MANGA_SERVER: {b}"),
        None => eprintln!("[diag] MANGA_SERVER: (未设置)"),
    }

    // 和 Worker 用的是同一套选取逻辑：按优先级逐个探活，取第一个通的
    let cands = candidate_bases(false, false);
    eprintln!("[diag] ---- 逐个探活（{} 个候选）----", cands.len());
    let mut chosen = pick_reachable(&cands);

    if chosen.is_none() {
        eprintln!("[diag] 候选都不通，试广播发现…");
        if let Some(d) = api::discover(api::DISCOVER_TIMEOUT) {
            eprintln!("[diag] 广播发现到 {}，探活…", d.base);
            if let Ok(info) = api::info(&d.base) {
                chosen = Some((d.base, info));
            }
        }
    }

    let Some((base, info)) = chosen else {
        eprintln!("[diag] 结论：一个可用地址都没有，找不到服务端");
        return;
    };
    eprintln!(
        "[diag] ---- 选用 {base}（服务端 v{}，屏幕 {}x{}）做完整自检 ----",
        info.version, info.screen.width, info.screen.height
    );
    eprintln!("[diag] base = {base}");

    fn step<T>(label: &str, f: impl FnOnce() -> Result<T, api::ApiError>) {
        let t0 = std::time::Instant::now();
        match f() {
            Ok(_) => eprintln!("[diag] {label:26} OK     {:>7}ms", t0.elapsed().as_millis()),
            Err(e) => eprintln!(
                "[diag] {label:26} 失败   {:>7}ms  {e}",
                t0.elapsed().as_millis()
            ),
        }
    }

    step("GET /api/info", || api::info(&base));
    step("GET /api/history", || api::history(&base));
    step("GET /api/album/228415", || api::album(&base, "228415"));
    step("GET /api/album/228415 再来", || api::album(&base, "228415"));
    step("GET /api/album/1478602", || api::album(&base, "1478602"));

    // 图片是二进制的，单独打一行，好确认大响应也能收全
    let t0 = std::time::Instant::now();
    match api::fetch_bytes(&base, "/img/228415/0") {
        Ok(Some(b)) => eprintln!(
            "[diag] {:26} OK     {:>7}ms  {} 字节  头={:02x?}",
            "/img/228415/0",
            t0.elapsed().as_millis(),
            b.len(),
            &b[..4.min(b.len())]
        ),
        Ok(None) => eprintln!("[diag] /img/228415/0 服务端说没有"),
        Err(e) => eprintln!(
            "[diag] {:26} 失败   {:>7}ms  {e}",
            "/img/228415/0",
            t0.elapsed().as_millis()
        ),
    }
}

/// 只做局域网发现，把结果打出来就退出。
///
/// 真机上排查「找不到服务端」时，这条命令能直接回答三个问题：
/// 广播发出去没有、有没有回包、回包里是什么。比在 UI 上猜快得多。
///
/// 用法：在 Kindle 的 shell 里跑
///   /mnt/us/extensions/kindle-manga/kindle-manga --discover
fn run_discover() {
    eprintln!(
        "[discover] 正在局域网里广播寻找服务端（最多 {} 秒）…",
        api::DISCOVER_TIMEOUT.as_secs()
    );
    match api::discover(api::DISCOVER_TIMEOUT) {
        Some(d) => {
            eprintln!("[discover] 找到服务端：{}", d.base);
            if let Some(s) = d.screen {
                eprintln!("[discover] 服务端屏幕基准 {}x{}", s.width, s.height);
            }
            eprintln!("[discover] 广播这条路是通的，客户端可以自动连上。");
        }
        None => {
            eprintln!("[discover] 没有收到任何响应。");
            eprintln!("[discover] 常见原因：");
            eprintln!(
                "[discover]   1. 服务端没在跑，或电脑防火墙挡住了 UDP {}",
                api::DEFAULT_PORT
            );
            eprintln!("[discover]   2. 不在同一个网段（UDP 广播一般过不了路由器）");
            eprintln!("[discover]   3. 环境禁了广播（QEMU slirp、部分云主机、部分企业 WiFi）");
            eprintln!("[discover] 兜底做法：直接指定地址，不依赖广播");
            eprintln!("[discover]   export MANGA_SERVER=http://<电脑IP>:8001");
            eprintln!("[discover]   /mnt/us/extensions/kindle-manga/kindle-manga");
        }
    }
}

/// 保存/查看/清除服务器地址，然后退出。
///
/// 在 Kindle 的墨水屏软键盘上戳一串 IP 很痛苦，SSH 或 KUAL 的 shell 里
/// 一行命令就搞定：
///
/// ```text
/// ./kindle-manga --set-server 192.168.1.91      # 保存（端口会自动补 8001）
/// ./kindle-manga --set-server 10.0.0.5:9000     # 带端口
/// ./kindle-manga --set-server ""                # 清除，回到自动发现
/// ./kindle-manga --set-server                   # 只查看当前存的是什么
/// ```
fn run_set_server(arg: Option<&str>) {
    match arg {
        None => {
            // 只查看
            match config_path() {
                Some(p) => {
                    eprintln!("[set-server] 配置文件：{}", p.display());
                    match saved_base() {
                        Some(b) => eprintln!("[set-server] 当前地址：{b}"),
                        None => eprintln!("[set-server] 当前没有保存地址（会用 MANGA_SERVER 或自动发现）"),
                    }
                }
                None => eprintln!("[set-server] 无法确定程序所在目录"),
            }
        }
        Some(raw) => {
            let normalized = api::normalize_base(raw);
            match save_base(&normalized) {
                Ok(()) => {
                    if normalized.is_empty() {
                        eprintln!("[set-server] 已清除手填地址，下次启动走自动发现");
                    } else {
                        eprintln!("[set-server] 已保存：{normalized}");
                        // 顺手探一下活，省得用户配了个错的地址还不知道
                        let t0 = std::time::Instant::now();
                        match api::info(&normalized) {
                            Ok(i) => eprintln!(
                                "[set-server] 探活 OK（{}ms），服务端 v{}，屏幕 {}x{}",
                                t0.elapsed().as_millis(),
                                i.version,
                                i.screen.width,
                                i.screen.height
                            ),
                            Err(e) => eprintln!(
                                "[set-server] ⚠️ 已保存，但探活失败：{e}\n\
                                 [set-server]    地址存下来了，下次启动会用它（连不上会自动换别的）"
                            ),
                        }
                    }
                }
                Err(e) => eprintln!("[set-server] 保存失败：{e}"),
            }
        }
    }
}

fn main() {
    init_logger();

    // 启动计时。用户反馈「应用要等好几秒才出来」，而这段时间里外只能看到
    // 一块 "Starting..." —— 到底是字体落盘慢、还是 Slint 初始化慢、还是
    // 首帧刷新慢，光看代码猜不出来。每个阶段打一行耗时，下次跑真机就有答案。
    let t_boot = std::time::Instant::now();
    let stage = |what: &str| {
        log::info!("[启动 +{}ms] {what}", t_boot.elapsed().as_millis());
    };

    // 这几个模式都不建窗口、不碰帧缓冲，所以**静态二进制也能跑**，
    // 方便在模拟器里预检、也方便在真机的 shell 里直接用。
    if std::env::args().any(|a| a == "--diag") {
        run_diag();
        return;
    }
    if std::env::args().any(|a| a == "--discover") {
        run_discover();
        return;
    }
    if let Some(i) = std::env::args().position(|a| a == "--set-server") {
        // 后面跟了参数就是设置，没跟就是查看。
        // 允许参数是空字符串（= 清除），所以只看「有没有下一个参数」。
        let arg = std::env::args().nth(i + 1);
        run_set_server(arg.as_deref());
        return;
    }

    let (platform, refresh) = install_backend();
    log::info!("平台后端: {platform}");
    // 这一步里面会把 2.9 MB 的字体写到 /tmp —— 这是启动路径上第一次真正的
    // 磁盘写入，值得量一下（Kindle 的 /tmp 不是 tmpfs，是落在闪存上的）。
    stage("后端安装完成（含字体落盘）");

    // 全屏刷新间隔：存过的值 > 默认值。
    // 在这里就设好，是因为后端进事件循环后会立刻开始渲染 —— 等 Worker 起来
    // 再设就晚了，第一屏可能已经按默认值刷过了。
    let full_refresh = saved_refresh_interval().unwrap_or(FULL_REFRESH_DEFAULT);
    refresh.set_interval(full_refresh);
    log::info!(
        "全屏刷新：{}（0 = 关闭；真机上才有，桌面预览是空壳）",
        match full_refresh {
            0 => "关闭".to_string(),
            n => format!("每 {n} 次刷新一次"),
        }
    );

    let local = local_pages();
    if local.is_some() {
        log::info!("本地单章模式：不连接服务端");
    }

    let app = AppWindow::new().expect("创建窗口失败");
    stage("窗口对象创建完成（还没渲染）");

    // 桌面预览：把窗口设成面板尺寸的一半，再配合 SLINT_SCALE_FACTOR=0.5，
    // 逻辑尺寸就正好是 1236×1648 —— 和 ui/app.slint 里那套绝对坐标对齐。
    //
    // 这两个数字必须成对使用，缺一个排版就不对：
    //   * 只设物理尺寸、缩放是 1.0 → 逻辑尺寸只有 618×824，界面被裁得只剩左上角
    //   * 只设缩放、不设物理尺寸 → 窗口按 .slint 的默认尺寸开，比例不对
    //
    // ⚠️ 这里**不马上读窗口尺寸**。Slint 的窗口适配器是延迟创建的（第一次渲染时
    // 才建），在那之前 `scale_factor()` 返回的是默认值 1.0，读出来会误判成
    // 「缩放没生效」。所以下面挂一个延时任务，等窗口真的出来了再报。
    #[cfg(not(all(feature = "kindle", target_arch = "arm")))]
    {
        use slint::ComponentHandle;
        let _ = app.window().set_size(slint::PhysicalSize::new(
            render::PANEL_W as u32 / 2,
            render::PANEL_H as u32 / 2,
        ));

        let weak = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(700), move || {
            let Some(app) = weak.upgrade() else { return };
            let win = app.window();
            let scale = win.scale_factor();
            let phys = win.size();
            let logical = phys.to_logical(scale);

            log::info!(
                "桌面窗口：物理 {}x{}  缩放 {}  逻辑 {:.0}x{:.0}",
                phys.width,
                phys.height,
                scale,
                logical.width,
                logical.height
            );

            if (logical.width - render::PANEL_W as f32).abs() > 1.0
                || (logical.height - render::PANEL_H as f32).abs() > 1.0
            {
                log::warn!(
                    "⚠️ 逻辑尺寸不是 {}x{}，界面排版会和真机不一致。\
                     检查 dev-ui.sh 里的 SLINT_SCALE_FACTOR=0.5 有没有传进来",
                    render::PANEL_W,
                    render::PANEL_H
                );
            } else {
                log::info!("逻辑尺寸与面板一致，排版和真机相同 ✓");
            }
        });
    }

    // 键盘三行不用在这里设 —— app.slint 里已有和 KB_LETTERS 一致的默认值，
    // Worker 第一次 emit 时会把模型补齐（字母页/符号页切换也走那里）。
    // v5 界面也没有 platform-name 属性了，平台名走「设置·关于」的 device-label。

    let (tx, rx) = channel::<Cmd>();

    // ---- 后台线程：独占状态，负责网络与解码，绝不阻塞 UI ----
    {
        let weak = app.as_weak();
        // 把刷新策略的句柄交给 Worker：用户在设置里改间隔时，
        // 它直接改就行 —— 内部是原子量，不用绕回 UI 线程。
        let refresh = refresh.clone();
        // worker 也要一条发送端：它自己会开线程取封面，
        // 取完从这条管道把结果送回来（见 Cmd::CoverReady）。
        let worker_tx = tx.clone();
        std::thread::spawn(move || {
            let mut worker = Worker::new(
                weak,
                local,
                refresh,
                full_refresh,
                platform.to_string(),
                worker_tx,
            );

            // 启动动作也走同一条流水线：本地模式直接进阅读器，
            // 服务端模式先去局域网里找服务端
            worker.handle(Cmd::Boot);

            while let Ok(cmd) = rx.recv() {
                let quit = matches!(cmd, Cmd::Quit);
                worker.handle(cmd);
                if quit {
                    return;
                }
            }
        });
    }

    // ---- UI 回调：只投递意图，一个阻塞动作都不做 ----
    macro_rules! wire {
        ($setter:ident, $cmd:expr) => {{
            let tx: Sender<Cmd> = tx.clone();
            app.$setter(move || {
                let _ = tx.send($cmd);
            });
        }};
    }

    wire!(on_quit_requested, Cmd::Quit);
    wire!(on_back, Cmd::Back);
    wire!(on_retry, Cmd::Retry);
    wire!(on_list_prev, Cmd::ListPrev);
    wire!(on_list_next, Cmd::ListNext);

    // 搜索 / 输入
    wire!(on_key_space, Cmd::KeySpace);
    wire!(on_key_backspace, Cmd::KeyBackspace);
    wire!(on_key_symbols, Cmd::KeySymbols);
    wire!(on_key_confirm, Cmd::KeyConfirm);
    wire!(on_kb_wanted, Cmd::KbWanted);
    wire!(on_kb_hide, Cmd::KbHide);

    // 设置
    wire!(on_set_edit_refresh, Cmd::SetEditRefresh);
    wire!(on_set_edit_server, Cmd::SetEditServer);
    wire!(on_set_recheck, Cmd::SetRecheck);
    wire!(on_set_clear_cache, Cmd::SetClearCache);
    wire!(on_page_left, Cmd::PageLeft);
    wire!(on_page_right, Cmd::PageRight);
    wire!(on_open_by_id, Cmd::OpenById);

    // 数字小键盘覆盖层
    wire!(on_keypad_backspace, Cmd::KeypadBackspace);
    wire!(on_keypad_clear, Cmd::KeypadClear);
    wire!(on_keypad_cancel, Cmd::KeypadCancel);
    wire!(on_keypad_ok, Cmd::KeypadOk);

    // 快速入门
    wire!(on_guide_prev, Cmd::GuidePrev);
    wire!(on_guide_next, Cmd::GuideNext);

    // 阅读器
    wire!(on_reader_menu_toggle, Cmd::ReaderMenuToggle);
    wire!(on_reader_fit_toggle, Cmd::ReaderFitToggle);
    wire!(on_reader_refresh, Cmd::ReaderRefresh);
    wire!(on_reader_prev, Cmd::ReaderPrev);
    wire!(on_reader_next, Cmd::ReaderNext);
    wire!(on_reader_goto_page, Cmd::ReaderGotoPage);
    wire!(on_reader_open_chapters, Cmd::ReaderOpenChapters);
    wire!(on_reader_star_toggle, Cmd::ReaderStarToggle);
    wire!(on_reader_exit, Cmd::ReaderExit);
    wire!(on_reader_hint_dismiss, Cmd::ReaderHintDismiss);

    // ---- 带参数的回调 ----
    {
        let tx = tx.clone();
        app.on_nav_picked(move |i| {
            let _ = tx.send(Cmd::NavPicked(i));
        });
    }
    {
        let tx = tx.clone();
        app.on_row_activated(move |idx| {
            let _ = tx.send(Cmd::Activate(idx));
        });
    }
    {
        let tx = tx.clone();
        app.on_star_hit(move |idx| {
            let _ = tx.send(Cmd::StarHit(idx));
        });
    }
    {
        let tx = tx.clone();
        app.on_key_char(move |k| {
            // 回调给的是字符本身（不是下标），直接带走
            let _ = tx.send(Cmd::KeyChar(k.to_string()));
        });
    }
    {
        let tx = tx.clone();
        app.on_keypad_digit(move |d| {
            // 同样是字符本身（"0".."9"）
            let _ = tx.send(Cmd::KeypadDigit(d.to_string()));
        });
    }
    {
        let tx = tx.clone();
        app.on_tag_activated(move |idx| {
            let _ = tx.send(Cmd::TagActivated(idx as usize));
        });
    }
    {
        let tx = tx.clone();
        app.on_ime_cand_hit(move |i| {
            let _ = tx.send(Cmd::ImePick(i as usize));
        });
    }
    {
        let tx = tx.clone();
        app.on_ime_page(move |d| {
            let _ = tx.send(Cmd::ImePage(d));
        });
    }
    {
        let tx = tx.clone();
        app.on_sort_picked(move |i| {
            let _ = tx.send(Cmd::SetSort(i as usize));
        });
    }
    {
        let tx = tx.clone();
        app.on_recent_picked(move |i| {
            let _ = tx.send(Cmd::RecentPicked(i.max(0) as usize));
        });
    }
    wire!(on_recent_clear, Cmd::RecentClear);
    {
        let tx = tx.clone();
        app.on_fav_tab_picked(move |i| {
            let _ = tx.send(Cmd::FavTabPicked(i));
        });
    }

    // ---- 接管电源键与休眠 ----
    //
    // 为什么必须由应用来做：launch.sh 把 Kindle 的 UI 框架冻住了，而电源键和
    // 合盖休眠的处理链路要穿过框架，冻住之后 powerd 会卡在「等框架回话」，
    // 于是电源键被忽略、合盖后触摸也失效。详见 src/power.rs 开头的说明。
    #[cfg(all(feature = "kindle", target_arch = "arm"))]
    {
        use slint::ComponentHandle;

        // 挂起前：把屏保铺到屏幕上，并**等它真的显示出来**。
        //
        // 为什么要等：电子墨水屏从「写进帧缓冲」到「肉眼看到」要几百毫秒
        // （EPDC 要走完一段波形）。如果铺完立刻挂起，屏幕会停在半张图上 ——
        // 或者因为刷新被打断而出现一块灰。
        //
        // 另一个更麻烦的点：Slint 是在事件循环里渲染的，set 完属性不等于画完了。
        // 所以这里分两步等：先等 UI 线程真的执行了那个闭包，再等 EPDC 刷完。
        let weak_pre = app.as_weak();
        let refresh_pre = refresh.clone();
        let before_suspend = move || {
            let Some(img) = screensaver::next_image() else {
                log::warn!("[屏保] 没有可用图片，这次不铺屏保（屏幕会停在当前画面）");
                return;
            };

            // 把一张图交给 UI 线程显示，并等它真的渲染出来。
            //
            // 为什么用 `put` 包一层而不是直接写两遍：下面要连着铺两张图
            // （先白后屏保），等待逻辑一模一样。
            //
            // ⚠️ 传进去的必须是 `PixelBuffer` 而不是 `slint::Image` ——
            // Image 不是 Send，只能在 UI 线程上组装。
            let put = |weak: slint::Weak<AppWindow>, buf: PixelBuffer| {
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        app.set_screensaver_image(slint::Image::from_rgba8(buf));
                        app.set_screensaver_shown(true);
                    }
                    let _ = tx.send(());
                });
                // 等 UI 线程收到（最多 2 秒，避免 UI 卡住时挂着不动）
                match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                    Ok(()) => log::info!("[屏保] 已交给 UI 线程渲染"),
                    Err(_) => log::warn!("[屏保] 等 UI 线程超时，可能界面卡住了"),
                }
            };

            // ---- 第一步：先铺纯白并全刷 ----
            //
            // 模仿 KOReader 的屏保：`ui/screensaver.lua` 里那句
            //   "On eInk ... flash the screen to white first, to eliminate ghosting."
            //
            // 为什么需要：挂起前屏幕上停的是最后一页漫画，而那常常是**大面积深色**。
            // 直接跳到屏保，一次 GC16 清不干净，残留就是「大大小小的色块」。
            // 先闪一下白把上一屏的电荷清掉，再画屏保就干净了。
            refresh_pre.force_next(); // 这一步必须全刷，不能走局部
            put(
                weak_pre.clone(),
                render::white_image(render::PANEL_W as u32, render::PANEL_H as u32),
            );
            std::thread::sleep(std::time::Duration::from_millis(900));

            // ---- 第二步：铺真正的屏保 ----
            refresh_pre.force_next();
            put(weak_pre.clone(), img);

            // 等电子墨水刷完。
            //
            // 取 1.5 秒 —— GC16 全刷在 PW5 上大约几百毫秒，
            // 加上 Slint 排到事件循环里渲染的时间，留足余量。
            // 宁可多等一会儿（一次休眠多花 1 秒没人会注意），
            // 也不要挂起时屏幕上还是半张图。
            std::thread::sleep(std::time::Duration::from_millis(1500));
            log::info!("[屏保] 等待刷新完成，可以挂起了");
        };

        // 唤醒后（或挂起失败时）：撤掉屏保，把界面恢复回来。
        let weak_post = app.as_weak();
        let after_wake = move || {
            let weak = weak_post.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    app.set_screensaver_shown(false);
                    // 挂起期间面板可能被动过（powerd 之类），重画一遍最稳
                    app.window().request_redraw();
                }
            });
        };

        power::spawn(before_suspend, after_wake);
    }

    stage("进入事件循环（首帧即将渲染）");
    app.run().expect("事件循环出错");
}

/// 极简 stderr logger，避免为了几行日志引入 env_logger。
fn init_logger() {
    struct StderrLogger;
    impl log::Log for StderrLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            eprintln!("[{}] {}", record.level(), record.args());
        }
        fn flush(&self) {}
    }
    static LOGGER: StderrLogger = StderrLogger;
    let _ = log::set_logger(&LOGGER);

    // 默认 info；`MANGA_LOG=debug` 可以放开。
    //
    // 加这个开关是因为「什么时候该做全屏刷新」这类决策只适合打在 debug 上：
    // 默认间隔是每 1 次刷新就全刷，用 info 打会把日志刷爆。
    // 和 MANGA_SERVER / MANGA_DIR / MANGA_NO_TIMEOUT 一样是环境变量开关。
    let level = match std::env::var("MANGA_LOG")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    };
    log::set_max_level(level);
}

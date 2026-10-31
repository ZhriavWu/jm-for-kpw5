//! 应用自己接管电源键与休眠。
//!
//! # 为什么必须自己管
//!
//! `launch.sh` 会把 Kindle 的 UI 框架 `killall -STOP` 掉（`awesome` / `cvm` /
//! `KPPMainApp`），否则它们会和我们抢帧缓冲。但**电源键和休眠的链路正好穿在框架上**：
//!
//! ```text
//! 按电源键 → powerd → 读 com.lab126.kaf 的 frameworkStarted 属性
//!                     ↑ 这个属性由 cvm 提供，而 cvm 被我们冻住了
//!           → 同步读等满 10 秒超时（lipc status=15）
//!           → powerd 判定「框架还没起来，还停在开机 splash」
//!           → 日志写下 "Ignoring power button"，按键被丢掉
//! ```
//!
//! 这不是推测，是从设备系统日志里读出来的：
//!
//! ```text
//! powerd: Power button pressed
//! powerd: E lipc:pto:timeout=10000, property=frameworkStarted, publisher=com.lab126.kaf
//! powerd: E def:lipc:LIPC access error ::status=15, propName=frameworkStarted
//! powerd: I def:pbisplash::Splash screen is on.  Ignoring power button
//! ```
//!
//! **合盖走的是同一条路**（霍尔传感器 → powerd → SCREEN SAVER）。框架冻着，
//! powerd 的状态机进得去出不来，卡在 SCREEN SAVER 里同时又吞掉触摸和电源键 ——
//! 实测表现就是「合盖几分钟再打开，屏幕还在但哪儿都点不动，只能强制重启」。
//!
//! 所以这两件事是同一个根因，得一起解决。
//!
//! # 怎么解决
//!
//! 1. `launch.sh` 里设 `preventScreenSaver 1`，让 powerd 别去走它走不完的那个
//!    状态机（退出时由 trap 恢复）—— 这一条直接堵住「合盖后卡死」
//! 2. 这个模块直接读 evdev：电源键按下、合盖 → `suspend-to-RAM`
//!
//! 这也是 `slint-backend-kindle` 作者预期的用法 —— 它的 `power.rs` 开头写着
//! 「powerd is not stopped, we just preempt powerd's own timer」。
//!
//! # 关于 EVIOCGRAB（独占）
//!
//! Slint 后端会用 `EVIOCGRAB` **独占触摸设备**（见它的 `touch.rs`，按
//! `ABS_MT_POSITION_X` 找触摸屏），所以它拿不到触摸的重复事件。
//!
//! 这里反过来：我们**只抓「报了 KEY_POWER 的节点」**，绝不碰触摸设备 ——
//! 一旦抢了触摸，应用的界面就点不动了。识别用的是能力位（`EVIOCGBIT`），
//! 不是节点编号，所以设备号怎么变都不会抓错。
//!
//! 抓下来有两个好处：电源键的事件只归我们；powerd 想管也管不了。
//! 万一抓不到（别人已经抓着），退化成「照常读」，并在日志里说清楚。

// 宿主上跑测试时这个模块也会编进来，但 spawn/scan/watch 在那时用不到
#![cfg_attr(not(all(feature = "kindle", target_arch = "arm")), allow(dead_code))]

use std::fs::{self, File};
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------- evdev 常量
//
// 取自 linux/input-event-codes.h，只列用得到的。

/// 按键类事件
const EV_KEY: u16 = 0x01;
/// 开关类事件（合盖/开盖走这个）
const EV_SW: u16 = 0x05;
/// 电源键
const KEY_POWER: u16 = 116;
/// 霍尔传感器 / 保护套盖子
const SW_LID: u16 = 0x00;

/// `_IOC` 宏：把方向、类型、序号、长度拼成 ioctl 请求号。
///
/// 与内核的 `_IOC(dir,type,nr,size) = (dir<<30)|(size<<16)|(type<<8)|nr` 对应。
///
/// 返回值用 `u32` 而不是 `c_ulong`：32 位 ARM 的 musl 里 `ioctl` 的请求参数是
/// `c_int`，x86_64 的 glibc 里是 `c_ulong`。存成 u32、调用处 `as _` 自适应，
/// 两个目标都能编过。
const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | (nr as u32)
}

/// `EVIOCGBIT(ev, len)`：查某类事件支持哪些位。
/// 内核定义：`_IOC(_IOC_READ, 'E', 0x20 + ev, len)`，`nr` 从 0x20 起。
const fn eviocgbit(ev: u16, len: usize) -> u32 {
    // _IOC_READ = 2
    ioc(2, b'E', (0x20 + ev) as u8, len)
}

/// `EVIOCGRAB`：独占设备。`_IOW('E', 0x90, int)`
const EVIOCGRAB: u32 = 0x4004_4590;

/// 一条 `input_event` 记录的长度。
///
/// 结构是 `{ timeval time; u16 type; u16 code; i32 value; }`。
/// 32 位 ARM 上 `timeval` 是 2×i32（8 字节），合计 16；64 位上是 2×i64，合计 24。
/// `type`/`code`/`value` 永远在**末尾 8 字节**，所以按位宽取长度，
/// 解析时从尾部往前找，不用管前面的时间戳。
#[cfg(target_pointer_width = "32")]
const EVENT_SIZE: usize = 16;
#[cfg(not(target_pointer_width = "32"))]
const EVENT_SIZE: usize = 24;

/// 从一条事件记录里取出 `(type, code, value)`。
fn parse_event(buf: &[u8]) -> Option<(u16, u16, i32)> {
    if buf.len() < EVENT_SIZE {
        return None;
    }
    let t = u16::from_ne_bytes([buf[EVENT_SIZE - 8], buf[EVENT_SIZE - 7]]);
    let c = u16::from_ne_bytes([buf[EVENT_SIZE - 6], buf[EVENT_SIZE - 5]]);
    let v = i32::from_ne_bytes([
        buf[EVENT_SIZE - 4],
        buf[EVENT_SIZE - 3],
        buf[EVENT_SIZE - 2],
        buf[EVENT_SIZE - 1],
    ]);
    Some((t, c, v))
}

// ---------------------------------------------------------------- 设备探测

/// 查设备是否支持某个事件类型/某个位。
///
/// 用 96 字节的位图，够覆盖到 `KEY_MAX`（0x2ff）—— `KEY_POWER` 是 116 位，
/// 落在第 14 个字节上。
fn has_bit(fd: RawFd, ev: u16, bit: u16) -> bool {
    let mut buf = [0u8; 96];
    let req = eviocgbit(ev, buf.len());
    // SAFETY: buf 是我们自己的定长数组，长度也一并传给了 ioctl 请求号
    let ret = unsafe { libc::ioctl(fd, req as _, buf.as_mut_ptr()) };
    if ret < 0 {
        return false;
    }
    let byte = (bit / 8) as usize;
    let mask = 1u8 << (bit % 8);
    byte < buf.len() && (buf[byte] & mask) != 0
}

/// 一个输入设备的用途。
struct Watched {
    file: File,
    path: PathBuf,
    /// 有电源键
    power: bool,
    /// 有盖子开关
    lid: bool,
}

/// 扫 `/dev/input/event*`，挑出有电源键或盖子开关的。
///
/// **不按节点编号硬编码**：设备号会随驱动加载顺序变化。
/// 按能力位筛选，换机型、换固件都不会抓错设备。
///
/// 特别地，**不会把触摸设备算进来** —— 触摸没有 `KEY_POWER` 位，
/// 而且 Slint 后端独占着它，抢过来界面就废了。
fn scan() -> Vec<Watched> {
    let mut out = Vec::new();

    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = fs::read_dir("/dev/input") {
        for e in rd.flatten() {
            let name = e.file_name();
            if name.to_string_lossy().starts_with("event") {
                paths.push(e.path());
            }
        }
    }
    paths.sort();

    for path in paths {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let fd = file.as_raw_fd();

        let power = has_bit(fd, EV_KEY, KEY_POWER);
        let lid = has_bit(fd, EV_SW, SW_LID);

        // 把探测结果全打出来 —— 真机上「电源键到底是哪个节点」只有设备知道，
        // 日志里留一份，以后换设备也不用重新猜。
        log::info!(
            "[power] {}: KEY_POWER={} SW_LID={}",
            path.display(),
            power,
            lid
        );

        if power || lid {
            out.push(Watched {
                file,
                path,
                power,
                lid,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- 挂起

/// 挂起失败时，把电源相关的内核状态全打出来。
///
/// `EBUSY` 基本只有两个来源：**有人持着 wake lock**，或者**刚按下的键还有未处理的
/// 唤醒事件**。这两样都能从下面这些文件看出来，不用再猜。
fn dump_power_state() {
    for p in [
        "/sys/power/state",
        "/sys/power/wakeup_count",
        "/sys/power/autosleep",
        "/sys/power/wake_lock",
    ] {
        match fs::read_to_string(p) {
            Ok(s) => log::warn!("[power]   {p} = {:?}", s.trim()),
            Err(e) => log::warn!("[power]   {p} 读不了: {e}"),
        }
    }

    if let Ok(rd) = fs::read_dir("/sys/power") {
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        log::warn!("[power]   /sys/power 下有: {}", names.join(" "));
    }

    for n in 0..3 {
        let p = format!("/sys/class/rtc/rtc{n}/wakealarm");
        if let Ok(s) = fs::read_to_string(&p) {
            log::warn!("[power]   {p} = {:?}", s.trim());
        }
    }
}

/// 内核的唤醒事件计数器。挂起前要先跟它「对账」，原因见 `suspend_once`。
const WAKEUP_COUNT: &str = "/sys/power/wakeup_count";

/// 挂起状态文件。写 `mem` 就睡。
const POWER_STATE: &str = "/sys/power/state";

/// 试着睡一次。成功的话这个调用**一直阻塞到设备被唤醒**，返回即是醒着。
///
/// # 为什么要两步
///
/// **按电源键这个动作本身就是一个唤醒事件**（`gpio-keys` 是内核唤醒源，
/// selftest 里能看到它的 `wakeup_count` 一直在涨）。内核发现「有未处理的唤醒事件」
/// 就会拒绝挂起，返回 `EBUSY` —— 这正是实测踩到的那个错误：
///
/// ```text
/// [power] 首次挂起被拒：Resource busy (os error 16)
/// ```
///
/// 内核文档给的标准解法是两步，powerd 自己也是这么做的：
///
/// 1. **读** `/sys/power/wakeup_count` 拿到当前计数，再**写回同一个值**。
///    这一步是「认领」：只有数值仍然相符才写得成功，等于原子地确认
///    「从现在这一刻起没有新的唤醒事件」。写失败就说明期间来了事件，得重来。
/// 2. 认领成功之后，pending 的唤醒事件已经被清掉，这时写 `state` 就能睡下去。
///
/// 直接用 `echo mem > /sys/power/state` 会跳第 1 步，所以必然 EBUSY。
fn suspend_once() -> std::io::Result<()> {
    // ---- 第 1 步：认领 wakeup_count ----
    match fs::read_to_string(WAKEUP_COUNT) {
        Ok(raw) => {
            let count = raw.trim().to_string();
            // 写回同一个值。数值对不上时内核会拒绝，这一步就失败了。
            fs::write(WAKEUP_COUNT, count.as_bytes())?;
            log::info!("[power] 已认领 wakeup_count={count}");
        }
        Err(e) => {
            // 老内核可能没有这个文件。没有就不认领，直接试第 2 步 ——
            // 大不了 EBUSY 然后重试，不影响正确性。
            log::info!("[power] 读不到 {WAKEUP_COUNT}（{e}），跳过认领直接挂起");
        }
    }

    // ---- 第 2 步：睡 ----
    log::info!("[power] 写 {POWER_STATE} = mem，等唤醒…");
    let r = fs::write(POWER_STATE, b"mem");
    if r.is_ok() {
        log::info!("[power] 已唤醒");
    }
    r
}

/// 挂起。失败会重试，重试到底还不行就把电源状态全打出来。
fn suspend_to_mem() -> std::io::Result<()> {
    let first = match suspend_once() {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    log::warn!("[power] 首次挂起被拒：{first}");

    // 每次写成功都会阻塞到唤醒，所以能走到下一轮就说明又失败了。
    for i in 1..=6u32 {
        std::thread::sleep(Duration::from_millis(300));
        log::info!("[power] 重试 #{i}…");
        match suspend_once() {
            Ok(()) => {
                log::info!("[power] 已唤醒（第 {i} 次重试成功）");
                return Ok(());
            }
            Err(e) => {
                // 错误码变了就值得记一笔 —— 说明卡住的原因在变
                if e.raw_os_error() != first.raw_os_error() {
                    log::warn!("[power] 重试 #{i} 换了个错误：{e}");
                }
            }
        }
    }

    log::warn!("[power] 重试 6 次都失败，把电源状态打出来：");
    dump_power_state();
    Err(first)
}

// ---------------------------------------------------------------- 事件循环

/// 挂起前 / 唤醒后要做的两件事。
///
/// 用两个回调而不是一个，是因为这两件事的**时机要求不一样**：
///
/// * `before_suspend` 必须在写 `/sys/power/state` **之前**跑完，而且得**等它画到屏幕上**。
///   电子墨水屏从「写帧缓冲」到「真的显示出来」要几百毫秒，early 挂起会看到半张图。
/// * `after_wake` 在醒之后跑，把界面恢复回来。
struct Hooks {
    before_suspend: Arc<dyn Fn() + Send + Sync>,
    after_wake: Arc<dyn Fn() + Send + Sync>,
}

/// 盯住一个设备。
fn watch(mut w: Watched, hooks: Arc<Hooks>) {
    let fd = w.file.as_raw_fd();

    // 尝试独占。抓到就没人能跟我们抢这个键；
    // 抓不到（比如 powerd 已经拿着）也不算失败，继续照常读。
    let grabbed = unsafe { libc::ioctl(fd, EVIOCGRAB as _, 1 as libc::c_int) } == 0;
    log::info!(
        "[power] 监听 {}（电源键={} 盖子={}）独占={}",
        w.path.display(),
        w.power,
        w.lid,
        grabbed
    );

    let mut buf = [0u8; EVENT_SIZE];
    let mut last_resume = Instant::now() - Duration::from_secs(10);

    /// 刚醒过来这段时间内不再挂起。
    ///
    /// 长按唤醒时手指还压着，会紧接着再来一个「按下」，不挡就会立刻又睡过去 ——
    /// 表现成「按一下醒了一下又没了」。powerd 也有同样的保护，
    /// 它的日志里写作 `Power button pressed too quickly(<2000ms). Ignoring`。
    const DEBOUNCE: Duration = Duration::from_millis(1500);

    loop {
        use std::io::Read;
        if let Err(e) = w.file.read_exact(&mut buf) {
            // 驱动重载、设备节点被换掉都会让读出错。
            // 不要退出线程 —— 睡一下重试，比让电源键永久失效好。
            log::warn!("[power] 读 {} 出错：{e}（1 秒后重试）", w.path.display());
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }

        let Some((ty, code, value)) = parse_event(&buf) else {
            continue;
        };

        let why: &'static str = match (ty, code, value) {
            // 电源键按下（1=按下 0=松开 2=长按重复）
            (EV_KEY, KEY_POWER, 1) if w.power => "电源键",
            // 合盖（1=合上）。开盖是 0，它是唤醒源，不在这里处理。
            (EV_SW, SW_LID, 1) if w.lid => "合盖",
            _ => continue,
        };

        if last_resume.elapsed() < DEBOUNCE {
            log::info!("[power] {why}，但刚醒不到 {DEBOUNCE:?}，忽略");
            continue;
        }

        log::info!("[power] {why} → 挂起");

        // ---- 挂起前：先把屏保铺到屏幕上 ----
        //
        // 顺序很要紧：powerd 原来的流程是「画屏保 → 挂起」，这一步由它负责，
        // 但它要穿过框架画，而框架被我们冻住了。所以自己补上，
        // 而且必须在写 /sys/power/state **之前** —— 挂起之后整个进程都被冻结，
        // 那时候再也画不了任何东西。
        (hooks.before_suspend)();

        match suspend_to_mem() {
            Ok(()) => {
                // 能走到这里就是已经被唤醒过了（suspend_to_mem 内部已经记了日志）
                last_resume = Instant::now();
                // 醒来撤掉屏保、把界面恢复回来
                (hooks.after_wake)();
            }
            Err(e) => {
                // 挂不起来不该让线程死掉 —— 否则以后电源键就彻底没反应了。
                // 但屏保已经画上去了，得撤掉，否则用户面对一张屏保却回不到界面。
                (hooks.after_wake)();
                log::warn!("[power] 这次挂起没成功（{e}），继续等按键");
                std::thread::sleep(Duration::from_millis(800));
            }
        }
    }
}

/// 启动电源键 / 盖子开关的监听。
///
/// * `before_suspend`：挂起**之前**调用，用来把屏保铺到屏幕上。
///   这个函数返回之后才会写 `/sys/power/state`，所以要保证它已经把画面
///   真正推上屏幕了（电子墨水屏从写帧缓冲到显示出来有几百毫秒延迟）。
/// * `after_wake`：唤醒之后调用（挂起失败时也会调，用来撤掉屏保）。
///
/// 找不到可监听的设备时只打警告就返回 —— 应用照常跑，只是电源键不受管，
/// **绝不会因为电源管理有问题而让应用起不来**。
pub fn spawn(
    before_suspend: impl Fn() + Send + Sync + 'static,
    after_wake: impl Fn() + Send + Sync + 'static,
) {
    let devices = scan();

    let n_power = devices.iter().filter(|d| d.power).count();
    let n_lid = devices.iter().filter(|d| d.lid).count();

    if devices.is_empty() {
        log::warn!("[power] 没找到带电源键/盖子的输入设备，电源键将不受管");
        return;
    }

    log::info!("[power] 匹配到 {} 个设备（电源键 {n_power}，盖子 {n_lid}）", devices.len());

    let hooks = Arc::new(Hooks {
        before_suspend: Arc::new(before_suspend),
        after_wake: Arc::new(after_wake),
    });

    for w in devices {
        let h = hooks.clone();
        std::thread::spawn(move || watch(w, h));
    }
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    /// 内核常量必须和 `linux/input-event-codes.h` 完全一致。
    ///
    /// 这四个数字错一个，表现就是「电源键永远没反应」或者「误把别的设备当电源键」，
    /// 而且**不会报任何错**。所以钉死。
    #[test]
    fn 内核常量正确() {
        assert_eq!(EV_KEY, 0x01, "EV_KEY");
        assert_eq!(EV_SW, 0x05, "EV_SW");
        assert_eq!(KEY_POWER, 116, "KEY_POWER");
        assert_eq!(SW_LID, 0x00, "SW_LID");
    }

    /// ioctl 请求号必须和内核 `_IOC` 宏算出来的逐位一致。
    ///
    /// 这里的期望值是按内核宏手算的：
    /// `_IOC(dir,type,nr,size) = (dir<<30)|(size<<16)|(type<<8)|nr`
    ///
    /// 算错的后果同上面一样：设备能力查询静默返回失败，
    /// 于是扫不到电源键设备，整个模块形同虚设。
    #[test]
    fn ioctl_请求号正确() {
        // _IOC_READ = 2, 'E' = 0x45
        // EVIOCGBIT(EV_KEY, 96) = _IOC(2, 'E', 0x20+1, 96)
        //   = (2<<30) | (96<<16) | (0x45<<8) | 0x21
        //   = 0x8000_0000 | 0x0060_0000 | 0x0000_4500 | 0x21
        //   = 0x8060_4521
        assert_eq!(eviocgbit(EV_KEY, 96), 0x8060_4521, "EVIOCGBIT(EV_KEY,96)");

        // EVIOCGBIT(EV_SW, 96) → nr = 0x20+5 = 0x25 → 0x8060_4525
        assert_eq!(eviocgbit(EV_SW, 96), 0x8060_4525, "EVIOCGBIT(EV_SW,96)");

        // EVIOCGRAB = _IOW('E', 0x90, int) = _IOC(1,'E',0x90,4)
        //   = (1<<30) | (4<<16) | (0x45<<8) | 0x90
        //   = 0x4000_0000 | 0x0004_0000 | 0x0000_4500 | 0x90
        //   = 0x4004_4590
        assert_eq!(EVIOCGRAB, 0x4004_4590, "EVIOCGRAB");
    }

    /// 事件记录的字节布局。
    ///
    /// `input_event` = `{ timeval time; u16 type; u16 code; i32 value; }`，
    /// 三个字段固定在末尾 8 字节。这里按两种位宽分别构造真实字节来验证 ——
    /// 偏移算错的话事件会被解析成乱七八糟的值，同样是静默失败。
    #[test]
    fn 事件解析() {
        // ---- 32 位布局：timeval 是 2×i32 = 8 字节，总长 16 ----
        let mut rec = [0u8; 16];
        // time 部分随便填，解析不该受它影响
        rec[0..8].copy_from_slice(&[0xAA; 8]);
        rec[8..10].copy_from_slice(&EV_KEY.to_ne_bytes());
        rec[10..12].copy_from_slice(&KEY_POWER.to_ne_bytes());
        rec[12..16].copy_from_slice(&1i32.to_ne_bytes());

        if EVENT_SIZE == 16 {
            let (t, c, v) = parse_event(&rec).expect("32 位布局应能解析");
            assert_eq!((t, c, v), (EV_KEY, KEY_POWER, 1), "32 位电源键按下");
        }

        // ---- 64 位布局：timeval 是 2×i64 = 16 字节，总长 24 ----
        let mut rec64 = [0u8; 24];
        rec64[0..16].copy_from_slice(&[0xBB; 16]);
        rec64[16..18].copy_from_slice(&EV_SW.to_ne_bytes());
        rec64[18..20].copy_from_slice(&SW_LID.to_ne_bytes());
        rec64[20..24].copy_from_slice(&1i32.to_ne_bytes());

        if EVENT_SIZE == 24 {
            let (t, c, v) = parse_event(&rec64).expect("64 位布局应能解析");
            assert_eq!((t, c, v), (EV_SW, SW_LID, 1), "64 位合盖");
        }

        // 短缓冲不该 panic
        assert!(parse_event(&[0u8; 4]).is_none(), "短缓冲应返回 None");
        assert!(parse_event(&[]).is_none(), "空缓冲应返回 None");
    }

    /// 能力位查询的位运算。
    ///
    /// `KEY_POWER=116` → 第 `116/8 = 14` 字节的第 `116%8 = 4` 位。
    /// 这个偏移算错就会挑到错误的设备。
    #[test]
    fn 能力位偏移() {
        let mut bitmap = [0u8; 96];
        // 手工点亮 KEY_POWER 那一位
        bitmap[KEY_POWER as usize / 8] |= 1u8 << (KEY_POWER as usize % 8);

        assert_eq!(KEY_POWER as usize / 8, 14, "KEY_POWER 应落在第 14 字节");
        assert_eq!(KEY_POWER as usize % 8, 4, "应落在第 4 位");
        assert_ne!(bitmap[14] & (1 << 4), 0, "该位应被点亮");

        // SW_LID = 0 → 第 0 字节第 0 位
        let mut sw = [0u8; 96];
        sw[SW_LID as usize / 8] |= 1u8 << (SW_LID as usize % 8);
        assert_ne!(sw[0] & 1, 0, "SW_LID 应在第 0 字节第 0 位");
    }
}

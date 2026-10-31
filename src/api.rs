//! 服务端接口层：局域网发现 + HTTP 调用 + 数据模型。
//!
//! 对应 `jm_kindle_raw.py` 提供的接口：
//!
//! ```text
//! GET  /api/info                探活，返回屏幕基准与接口清单
//! GET  /api/search?q=&o=&p=     搜索（p 从 1 开始）
//! GET  /api/album/<album_id>    本子详情，含章节目录
//! GET  /api/photo/<photo_id>    章节详情，含每页图片地址
//! GET  /api/history             最近观看
//! POST /api/history             写一条观看记录 {id, title}
//! GET  /img/<photo_id>/<page>   第 page 页图片，**页码从 0 开始**
//! GET  /cover/<album_id>        封面（本来就是 jpg）
//! ```
//!
//! 两个约定必须记牢：
//!
//! 1. **图片页码从 0 开始**，越界或无此章节一律回 404。客户端靠 404 判断
//!    「翻到末尾了」，不是错误。
//! 2. **/api/* 的响应都带 `ok` 字段**，false 时看 `error`；出错时 HTTP 状态码
//!    也是 4xx/5xx —— 错误正文里那条 `error` 信息比状态码有用得多，得读出来。
//!
//! 关于 HTTPS：这里的手写 HTTP 客户端只支持明文 http。内网直连和 Tailscale
//! 组网都走明文，够用；要连公网 HTTPS 反代得再加 TLS（rustls 交叉编译到
//! armv7-musl 代价不小），需要时再说。

use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- 常量

/// 服务端默认端口，与 `jm_kindle_raw.py` 的 `PORT` 一致。
/// 注意它和 `jm_kindle_server.py`(8000) 是故意错开的，两个可以同时跑。
pub const DEFAULT_PORT: u16 = 8001;

/// 局域网发现的广播魔术字，必须和服务端的 `DISCOVER_MAGIC` 逐字节相同。
const DISCOVER_MAGIC: &[u8] = b"JMKINDLE_DISCOVER";

/// 发现用的 UDP 端口。UDP/TCP 各自独立，所以和服务同号不冲突。
const DISCOVER_PORT: u16 = DEFAULT_PORT;

/// 探活、历史这类「服务端纯本地读取」的接口超时。这些接口立刻返回。
const FAST_TIMEOUT: Duration = Duration::from_secs(20);

/// 会打到 jmcomic 上游的接口超时（搜索、本子详情、章节详情）。
///
/// 给这么宽是因为**首次请求特别慢**：服务端要先做域名探测（`刷新 APP 域名`）、
/// 建立到上游的 TLS、再等它响应。实测服务端冷启动后第一次 `/api/album`
/// 超过 15 秒，用 15 秒超时会在服务端还没答之前就把请求掐掉 ——
/// 那样连「本子不存在」这种正常的业务错误都收不到。
///
/// 参考值来自服务端自己的 `_probe.py`，它用的也是 `timeout=120`。
const SLOW_TIMEOUT: Duration = Duration::from_secs(120);

/// 取图超时。服务端可能在现下载 + 转码，第一次取某页会明显慢，给宽松些。
const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);

/// 服务端开起来之后探活等多久。
pub const DISCOVER_TIMEOUT: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------- 错误

#[derive(Debug)]
pub enum ApiError {
    /// 服务端说没有这个东西（404）。翻过末尾就是靠它判断的，不是故障。
    End,
    /// 网络不通、连不上、超时，或者服务端返回了 4xx/5xx 的错误正文。
    Net(String),
    /// 通了，但回来的不是我们能认的 JSON。
    Bad(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::End => write!(f, "没有更多了"),
            ApiError::Net(m) => write!(f, "{m}"),
            ApiError::Bad(m) => write!(f, "{m}"),
        }
    }
}

/// 服务端出错时的响应体：`{"ok": false, "error": "..."}`。
#[derive(Deserialize)]
struct ErrBody {
    error: Option<String>,
}

/// 从服务端的错误响应体里挖出 `error` 字段。
/// 服务端写的那句话比「HTTP 500」有用得多，值得费这点事。
fn server_error_msg(code: u16, body: &[u8]) -> ApiError {
    if code == 404 {
        // 404 统一当「到头了」处理，调用方不用区分端点
        return ApiError::End;
    }
    let msg = serde_json::from_slice::<ErrBody>(body)
        .ok()
        .and_then(|b| b.error)
        .unwrap_or_else(|| format!("服务端返回 HTTP {code}"));
    ApiError::Net(msg)
}

// ---------------------------------------------------------------- 数据模型

/// `/api/info`
///
/// 字段按服务端返回的结构补全，其中一部分客户端暂时用不上 ——
/// 留着是为了「服务端加了什么字段、这里一眼能对上」，而不是漏了。
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Info {
    pub service: String,
    pub version: String,
    /// 服务端认为「你是用哪个地址访问到我的」，取自请求的 Host 头
    pub base_url: String,
    pub screen: ScreenSize,
    pub search_per_page: u32,
    pub default_sort: String,
    /// 服务端支持的排序方式（值 + 中文名）。
    ///
    /// **由服务端说了算，客户端不写死** —— 排序参数最终是转发给 jmcomic 的，
    /// 只有服务端知道它认哪些值。我们这边只负责把 `value` 原样回传。
    ///
    /// `#[serde(default)]` 是为了兼容旧版服务端：没有这个字段时是空表，
    /// 界面上就不显示排序按钮（而不是报错连不上）。
    #[serde(default)]
    pub sort_options: Vec<SortOption>,
}

/// `/api/info` 里的一项排序方式。
#[derive(Deserialize, Clone)]
pub struct SortOption {
    /// 回传给服务端的排序值，如 `mr` / `tr`
    pub value: String,
    /// 显示名，如「最新」「最高评分」
    pub label: String,
}

#[derive(Deserialize, Clone, Copy)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

/// `/api/search`
///
/// ⚠️ `page_count` / `total` 都必须是 `Option`：上游 jmcomic 有时不返回总数，
/// 服务端那两处就原样是 `null`（见 `jm_kindle_raw.py` 的 `search_slice`，
/// `display_pages` 初值就是 None）。写成 `u32` 会在搜索时直接反序列化失败 ——
/// 实测踩过：`invalid type: null, expected u32`。
#[derive(Deserialize)]
pub struct SearchResult {
    pub keyword: String,
    pub page: u32,
    #[serde(default)]
    pub page_count: Option<u32>,
    #[serde(default)]
    pub total: Option<u32>,
    #[serde(default)]
    pub items: Vec<SearchItem>,
}

#[derive(Deserialize)]
pub struct SearchItem {
    pub id: String,
    pub title: String,
    /// 相对路径（没加 abs=1）或完整 URL，两种都要能接
    #[serde(default)]
    pub cover: String,
}

/// `/api/album/<id>`
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Album {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub cover: String,
    pub page_count: u32,
    pub chapter_count: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    /// **角色**（作品里出场的人物），如 `["森亚露露卡"]`。
    ///
    /// `#[serde(default)]` 不能省：旧服务端（Python 版）没有这个字段，
    /// 缺了它整个 `/api/album` 反序列化失败 —— 表现是「本子打不开」。
    #[serde(default)]
    pub actors: Vec<String>,
    /// **作品**（原作 / 系列），如 `["名侦探光之美少女"]`。
    /// 同样要 default。
    #[serde(default)]
    pub works: Vec<String>,
    pub chapters: Vec<Chapter>,
}

#[derive(Deserialize)]
pub struct Chapter {
    pub id: String,
    /// 服务端原样透传 jmcomic 的 `album_index`，可能是个数字也可能是个字符串
    #[serde(deserialize_with = "de_text")]
    pub index: String,
    pub title: String,
    #[serde(default)]
    pub indextitle: String,
}

/// `/api/photo/<id>`
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct Photo {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub indextitle: String,
    /// 本章在目录里的序号（1 基）。用来算「下一章」。
    #[serde(default, deserialize_with = "de_text")]
    pub index: String,
    #[serde(default)]
    pub album_id: String,
    pub page_count: u32,
    /// 服务端明说页码从 0 开始，这里存下来只是给文档一个落点
    #[serde(default)]
    #[allow(dead_code)]
    pub page_base: i64,
    /// 禁漫的图片重排 ID。**没这个字段 = 不用解密**。
    ///
    /// 旧版 Python 服务端发的是已经解好密的图，所以这条字段缺失时客户端
    /// 必须原样显示（见 `descramble.rs` 顶部的兼容说明）。
    #[serde(default, deserialize_with = "de_opt_u64")]
    pub scramble_id: Option<u64>,
    pub images: Vec<ImageRef>,
}

#[derive(Deserialize)]
pub struct ImageRef {
    #[allow(dead_code)]
    pub page: u32,
    /// 服务端保持原格式（多半是 webp）
    pub url: String,
    /// 同一张图转成 JPEG 的地址，解不动原格式时用它兜底
    #[serde(default)]
    pub jpeg_url: String,
    /// 本页的分割数（**服务端直接算好最省事**）。给了就用，不再自己算。
    #[serde(
        default,
        alias = "seg_num",
        alias = "segmentation",
        deserialize_with = "de_opt_u32"
    )]
    pub seg: Option<u32>,
    /// 原始文件名（不含后缀，如 `00001`）。服务端不直接给 `seg` 时，
    /// 客户端用它 + `scramble_id` 算分割数。
    #[serde(default, alias = "img_file_name", alias = "name")]
    pub filename: String,
}

/// `/api/history`
#[derive(Deserialize)]
pub struct HistoryResp {
    pub items: Vec<HistoryItem>,
}

#[derive(Deserialize, Serialize)]
pub struct HistoryItem {
    pub id: String,
    pub title: String,
}

/// 把「数字或字符串」都收成字符串。
///
/// jmcomic 的 `album_index` 到底是 int 还是 str 没有保证，服务端又是原样透传的。
/// 与其赌，不如两种都接 —— JSON 里写 `"index": 3` 或 `"index": "3"` 都不炸。
fn de_text<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    })
}

/// 把「数字 / 字符串 / null」都收成 `Option<u64>`。
///
/// 服务端可能把 `scramble_id` 写成 `220980` 或 `"220980"`（jmcomic 自己
/// 存的就是字符串），两种都接；缺字段或 `null` 就是 `None`。
fn de_opt_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// 同上，收成 `Option<u32>`（分割数用）。
fn de_opt_u32<'de, D>(d: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(de_opt_u64(d)?.and_then(|v| u32::try_from(v).ok()))
}

// ---------------------------------------------------------------- URL 处理

/// 把服务端给的地址拼成完整 URL。
///
/// 服务端默认返回相对路径（`/cover/123`），只有带 `abs=1` 时才补全 ——
/// 但补全用的 Host 头在某些反代下会被改写，所以**拼接交给客户端做**更稳。
/// 已经是完整 URL 的原样返回。
pub fn join(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// 最小百分号编码，只保留 RFC 3986 的 unreserved 字符。
///
/// 不复用现成 crate：搜索词常含中日文，必须按 UTF-8 逐字节转义，
/// 而这条规则短到不值得引一个依赖。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- HTTP 传输
//
// 这里**不用 ureq**，而是直接用 std::net 手写。原因不是嫌它不好，是实测踩了坑：
//
// 在模拟器里（QEMU + LD_PRELOAD 的 fbshim/touchshim）观察到一种现象 ——
// 同一个接口、同一份代码，`--diag` 模式（不建窗口）二十次都成功、20~30 ms 回来；
// 一旦跑起 UI，请求就在 **20~30 ms** 报 `timed out reading response`，
// 而超时阈值明明是 120 秒。
//
// 扒 ureq 的源码定位到：那个错误串有两个来源，一是 `time_until_deadline`
// 发现 deadline 已过，二是 `fill_buf` 收到 `ErrorKind::WouldBlock`。
// 又做了对照实验：把 `.timeout()` 整个关掉（`MANGA_NO_TIMEOUT=1`），
// **错误照旧** —— 说明不是它的超时机制，而是 socket 在读的时候被当成了非阻塞。
//
// 与其继续跟 ureq 在这套 shim 环境下的 socket 处理较劲，不如换成手写：
// 逻辑透明、行为完全可控，顺带还少一个依赖 —— 对 Kindel 上 7 MB 的静态二进制
// 来说，去掉 ureq 连带那一串依赖是实打实的体积收益。
//
// 服务端是 HTTP/1.0 且一律带 Content-Length，所以我们发 `Connection: close`
// 然后一路读到 EOF 就够了，不需要处理分块编码。

/// 请求方法
enum Method {
    Get,
    Post(&'static str),
}

/// 一次 HTTP 往返的结果。
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

/// 把 `http://host:port/path` 拆成连接用的地址和请求行里的路径。
fn parse_http_url(url: &str) -> Result<(String, u16, String), ApiError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| ApiError::Bad(format!("只支持 http:// 地址：{url}")))?;

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    // rsplit_once 而不是 split：IPv6 字面量里有冒号（虽然这里用不到，
    // 但写对了不花钱）
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| ApiError::Bad(format!("端口不对：{p}")))?,
        ),
        None => (authority.to_string(), 80),
    };

    if host.is_empty() {
        return Err(ApiError::Bad(format!("地址里没有主机名：{url}")));
    }
    Ok((host, port, path.to_string()))
}

/// 带超时的 TCP 连接。
///
/// # 为什么不用 `TcpStream::connect_timeout`
///
/// 它的实现是：把 socket 设成**非阻塞** → 发起 connect → poll 等超时 → **设回阻塞**。
/// 在 Kindle 模拟器那套 QEMU + LD_PRELOAD shim 环境里，最后那步「设回阻塞」没有
/// 生效，于是之后每次 read 都立刻返回 EAGAIN，看起来像「读超时」但其实是
/// 1 毫秒就失败了。这个坑查了很久，详见本文件开头的说明。
///
/// # 这里的做法
///
/// 在**独立线程**里做普通的阻塞 connect，主线程用 channel 等到 deadline 就放弃。
/// 好处是完全不碰 socket 的非阻塞标志，行为可预测；代价是超时后那个线程还会
/// 挂一会儿，等系统自己的 connect 超时（通常是几十秒）才结束。
///
/// 对一个只在启动时连一两次的客户端来说，这个代价完全可以接受 ——
/// 换来的确定性更重要。
fn connect_with_timeout(addr: SocketAddr, timeout: Duration) -> Result<TcpStream, ApiError> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // 连接结果丢回来；主线程不要了就 send 失败，忽略即可
        let _ = tx.send(TcpStream::connect(addr));
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(sock)) => Ok(sock),
        Ok(Err(e)) => Err(ApiError::Net(format!("连不上 {addr}：{e}"))),
        Err(_) => Err(ApiError::Net(format!(
            "连接 {addr} 超时（{}秒）。地址可能不对，或电脑的防火墙挡住了",
            timeout.as_secs()
        ))),
    }
}

/// 真正发一次请求。每次都是新连接 —— 服务端是 HTTP/1.0，本来就不保持连接。
fn http_request(url: &str, method: Method, body: Option<(&str, &[u8])>, timeout: Duration) -> Result<HttpResponse, ApiError> {
    let (host, port, path) = parse_http_url(url)?;

    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| ApiError::Net(format!("解析 {host} 失败: {e}")))?
        .next()
        .ok_or_else(|| ApiError::Net(format!("{host} 解析不出地址")))?;

    // 连接要有超时，但又不能碰 socket 的非阻塞标志 —— 所以走这个包装（原因见它的注释）
    let connect_timeout = timeout.min(Duration::from_secs(12));
    let mut sock = connect_with_timeout(addr, connect_timeout)?;

    sock.set_read_timeout(Some(timeout))
        .map_err(|e| ApiError::Net(format!("设置读超时失败: {e}")))?;
    sock.set_write_timeout(Some(timeout))
        .map_err(|e| ApiError::Net(format!("设置写超时失败: {e}")))?;
    // 局域网里 Nagle 只会白白增加延迟，关掉
    let _ = sock.set_nodelay(true);

    let (method_line, extra, payload): (&str, String, &[u8]) = match &method {
        Method::Get => ("GET", String::new(), &[]),
        Method::Post(ctype) => (
            "POST",
            format!("Content-Type: {ctype}\r\nContent-Length: {}\r\n", body.map(|(_, b)| b.len()).unwrap_or(0)),
            body.map(|(_, b)| b).unwrap_or(&[]),
        ),
    };

    let mut req = format!(
        "{method_line} {path} HTTP/1.0\r\n\
         Host: {host}:{port}\r\n\
         User-Agent: kindle-manga/0.1\r\n\
         Accept: */*\r\n\
         {extra}\
         Connection: close\r\n\
         \r\n"
    )
    .into_bytes();
    req.extend_from_slice(payload);

    let started = Instant::now();
    sock.write_all(&req)
        .map_err(|e| ApiError::Net(format!("发送请求失败: {e}")))?;
    sock.flush().ok();

    // 服务端每次都带 Content-Length 并在 HTTP/1.0 下关连接，
    // 所以读到 EOF 就是完整响应
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).map_err(|e| {
        // 把「等了多久」写进错误里。
        // 这个数字是区分两种故障的关键：
        //   等到 ~超时值   → 响应没回来（服务端慢 / 网络被卡住）
        //   几乎立刻返回   → socket 是非阻塞的，read 直接 EAGAIN
        ApiError::Net(format!(
            "读响应失败（等了 {}ms）：{e}",
            started.elapsed().as_millis()
        ))
    })?;

    if raw.is_empty() {
        return Err(ApiError::Net("服务端没有返回任何内容".into()));
    }

    // 头和体按第一个空行切开
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| ApiError::Bad("响应里找不到头部结束标记".into()))?;
    let head = &raw[..split];
    let body = raw[split + 4..].to_vec();

    // 状态行形如 "HTTP/1.0 200 OK"
    let first_line_end = head.iter().position(|&b| b == b'\r').unwrap_or(head.len());
    let status_line = String::from_utf8_lossy(&head[..first_line_end]);
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| ApiError::Bad(format!("状态行看不懂：{status_line}")))?;

    Ok(HttpResponse { status, body })
}

/// GET 一个 JSON 接口。
fn get_json<T: DeserializeOwned>(base: &str, path: &str, timeout: Duration) -> Result<T, ApiError> {
    let url = join(base, path);
    let resp = http_request(&url, Method::Get, None, timeout)?;
    if resp.status >= 400 {
        return Err(server_error_msg(resp.status, &resp.body));
    }
    serde_json::from_slice::<T>(&resp.body)
        .map_err(|e| ApiError::Bad(format!("返回的不是预期 JSON: {e}")))
}

/// 探活。顺便拿到服务端声明的屏幕基准，可以和自己实现的对一下。
pub fn info(base: &str) -> Result<Info, ApiError> {
    get_json(base, "/api/info", FAST_TIMEOUT)
}

pub fn search(base: &str, keyword: &str, order: &str, page: u32) -> Result<SearchResult, ApiError> {
    get_json(
        base,
        &format!(
            "/api/search?q={}&o={}&p={}",
            urlencode(keyword),
            urlencode(order),
            page
        ),
        SLOW_TIMEOUT,
    )
}

pub fn album(base: &str, album_id: &str) -> Result<Album, ApiError> {
    get_json(base, &format!("/api/album/{}", urlencode(album_id)), SLOW_TIMEOUT)
}

pub fn photo(base: &str, photo_id: &str) -> Result<Photo, ApiError> {
    get_json(base, &format!("/api/photo/{}", urlencode(photo_id)), SLOW_TIMEOUT)
}

pub fn history(base: &str) -> Result<Vec<HistoryItem>, ApiError> {
    let r: HistoryResp = get_json(base, "/api/history", FAST_TIMEOUT)?;
    Ok(r.items)
}

/// 写一条观看记录。
///
/// 打开本子时服务端自己也会记（`api_album` 里调了 `save_history`），
/// 这里是在**打开章节**时补一次，好让「最近观看」的排序跟着实际阅读走。
/// 失败不影响阅读，调用方忽略返回值即可。
pub fn post_history(base: &str, id: &str, title: &str) -> Result<(), ApiError> {
    let body = serde_json::to_vec(&HistoryItem {
        id: id.to_string(),
        title: title.to_string(),
    })
    .map_err(|e| ApiError::Bad(e.to_string()))?;

    let url = join(base, "/api/history");
    let resp = http_request(
        &url,
        Method::Post("application/json"),
        Some(("application/json", &body)),
        FAST_TIMEOUT,
    )?;

    // 正文是更新后的历史列表，这里不关心内容；但状态码要看，出错了得让调用方知道
    if resp.status >= 400 {
        return Err(server_error_msg(resp.status, &resp.body));
    }
    Ok(())
}

/// `/api/cache` 的响应。
#[derive(Deserialize, Clone, Debug, Default)]
pub struct CacheStats {
    /// 精确到字节的用量。界面只用 MB/GB，留着是因为排查问题时
    /// 「到底差了多少字节」比两位小数的 MB 有用。
    #[serde(default)]
    #[allow(dead_code)]
    pub used_bytes: u64,
    #[serde(default)]
    pub used_mb: u64,
    /// 0 = 不限制
    #[serde(default)]
    pub limit_mb: u64,
    /// 缓存了多少个文件（排查用，界面不显示）
    #[serde(default)]
    #[allow(dead_code)]
    pub files: u64,
}

impl CacheStats {
    /// 设置页那一行要显示的文字。
    ///
    /// 用量大于 1 GB 时用 GB 显示，否则用 MB —— 「已用 312 MB / 1 GB」
    /// 比「已用 312 MB / 1024 MB」好读。
    pub fn describe(&self) -> String {
        let used = if self.used_mb >= 1024 {
            format!("{:.1} GB", self.used_mb as f64 / 1024.0)
        } else {
            format!("{} MB", self.used_mb)
        };
        let limit = if self.limit_mb == 0 {
            "不限制".to_string()
        } else if self.limit_mb >= 1024 {
            format!("{} GB", self.limit_mb / 1024)
        } else {
            format!("{} MB", self.limit_mb)
        };
        format!("已用 {used} / 上限 {limit}")
    }
}

fn cache_request(base: &str, body: Option<Vec<u8>>) -> Result<CacheStats, ApiError> {
    let url = join(base, "/api/cache");
    let resp = match &body {
        Some(b) => http_request(
            &url,
            Method::Post("application/json"),
            Some(("application/json", b)),
            FAST_TIMEOUT,
        )?,
        None => http_request(&url, Method::Get, None, FAST_TIMEOUT)?,
    };
    if resp.status >= 400 {
        return Err(server_error_msg(resp.status, &resp.body));
    }
    serde_json::from_slice::<CacheStats>(&resp.body)
        .map_err(|e| ApiError::Bad(format!("缓存接口返回的不是预期 JSON: {e}")))
}

/// 查磁盘缓存用量。
pub fn cache_stats(base: &str) -> Result<CacheStats, ApiError> {
    cache_request(base, None)
}

/// 改缓存上限（MB，0 = 不限制）。服务端会立刻按新上限清理一次。
pub fn set_cache_limit(base: &str, mb: u32) -> Result<CacheStats, ApiError> {
    let body = format!("{{\"max_mb\":{mb}}}").into_bytes();
    cache_request(base, Some(body))
}

/// 清空磁盘缓存。
pub fn clear_cache(base: &str) -> Result<CacheStats, ApiError> {
    cache_request(base, Some(b"{\"clear\":true}".to_vec()))
}

/// 带重试地跑一个请求，返回结果和**实际用掉的尝试次数**。
///
/// # 为什么需要这个
///
/// 在模拟器里观察到一种很别扭的抖动：一模一样的请求，孤立跑（`--diag`，
/// 不建窗口）二十次二十次都成功、20~30 ms 回来；一旦跑起 UI，就时不时在
/// **30 ms 上下**报 `timed out reading response` —— 而超时阈值是 120 秒。
/// 三十毫秒就报超时显然不是「等久了」，更像是连接被中途掐断。
///
/// 这事查了很久，`--diag` 证明服务端、网络、shim、ARM 代码生成、ureq 都没问题，
/// 只有「渲染 + 网络同时进行」时才犯。真机上没有 virtio-serial 这条通道，
/// 大概率不会遇到 —— 但在模拟器里它让开发寸步难行。
///
/// 与其继续追这个模拟器特有的抖动，不如**把它当成一次偶发网络故障**来处理：
/// 重试一次基本就过了。耗时统计也一并返回，好让调用方记录到底重试过几次。
pub fn retry<T>(
    attempts: u32,
    mut f: impl FnMut() -> Result<T, ApiError>,
) -> Result<(T, u32), ApiError> {
    let mut last = None;
    for i in 0..attempts.max(1) {
        match f() {
            Ok(v) => return Ok((v, i + 1)),
            Err(e) => {
                // 「到头了」是正常业务结果，不该重试
                if matches!(e, ApiError::End) {
                    return Err(e);
                }
                if i + 1 < attempts {
                    std::thread::sleep(Duration::from_millis(250 * (i as u64 + 1)));
                }
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or(ApiError::Net("请求失败".into())))
}

/// 取一份二进制（图片）。
///
/// `Ok(None)` 表示服务端明确说「没有」—— 图片接口越界就是这种情况，
/// 是正常的翻页边界，不是错误。调用方据此判断「到头了」。
pub fn fetch_bytes(base: &str, url_or_path: &str) -> Result<Option<Vec<u8>>, ApiError> {
    let url = join(base, url_or_path);
    let resp = http_request(&url, Method::Get, None, IMAGE_TIMEOUT)?;

    // 越界 → 服务端回 404，这就是「到头了」，不是错误
    if resp.status == 404 {
        return Ok(None);
    }
    if resp.status >= 400 {
        return Err(server_error_msg(resp.status, &resp.body));
    }
    Ok(if resp.body.is_empty() {
        None
    } else {
        Some(resp.body)
    })
}

// ---------------------------------------------------------------- 局域网发现

#[derive(Deserialize)]
struct DiscoverReply {
    service: String,
    #[allow(dead_code)]
    version: Option<String>,
    port: Option<u16>,
    #[allow(dead_code)]
    path: Option<String>,
    screen: Option<ScreenSize>,
}

/// 找到的服务端。
pub struct Discovered {
    /// 形如 `http://192.168.1.20:8001`
    pub base: String,
    /// 服务端自报的屏幕基准。客户端按自己的面板尺寸渲染，
    /// 这个值目前只用来和日志对一下，确认没连错设备。
    #[allow(dead_code)]
    pub screen: Option<ScreenSize>,
}

/// 往局域网广播喊一句，问服务端在哪。
///
/// 原理很朴素：服务端收到 `JMKINDLE_DISCOVER` 就回一条 JSON，客户端**从收到
/// 回包的源地址**里读出服务端 IP。好处是服务端全程不需要知道自己的 IP，
/// 换网络、换路由器地址也不用改任何配置。
///
/// 找不到就返回 `None`（超时或局域网禁广播），调用方应引导用户手填地址。
pub fn discover(timeout: Duration) -> Option<Discovered> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.set_broadcast(true).ok()?;
    // 单次等 400ms，整体到 deadline 为止 —— 这样能在等待期内反复重试收包
    sock.set_read_timeout(Some(Duration::from_millis(400))).ok()?;

    for target in broadcast_targets() {
        // 发失败不是致命问题（比如该网卡不支持广播），继续试下一个
        let _ = sock.send_to(DISCOVER_MAGIC, (target, DISCOVER_PORT));
    }

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let reply = match serde_json::from_slice::<DiscoverReply>(&buf[..n]) {
                    Ok(r) => r,
                    Err(_) => continue, // 不是我们的回包，忽略
                };
                if reply.service != "jm-kindle" {
                    continue;
                }
                let ip = match from.ip() {
                    IpAddr::V4(v4) => v4,
                    IpAddr::V6(_) => continue,
                };
                let port = reply.port.unwrap_or(DEFAULT_PORT);
                return Some(Discovered {
                    base: format!("http://{ip}:{port}"),
                    screen: reply.screen,
                });
            }
            // 超时/被中断都只是「这一轮没收到」，继续等到 deadline
            Err(_) => continue,
        }
    }
    None
}

/// 要往哪些地址发广播。
///
/// 受限广播 `255.255.255.255` 不是所有系统都会送到默认网卡上，所以再补一个
/// 按本机主地址推出来的子网广播。局域网基本都是 /24，够用；推不出来就只发
/// 受限广播，不影响别的路径。
fn broadcast_targets() -> Vec<Ipv4Addr> {
    let mut out = vec![Ipv4Addr::BROADCAST];

    // 借一个「连外网」的 UDP socket 让系统挑出主网卡地址。
    // UDP 不握手，connect 只是选路，不会真的发包。
    if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                let o = a.ip().octets();
                let b = Ipv4Addr::new(o[0], o[1], o[2], 255);
                if !out.contains(&b) {
                    out.push(b);
                }
            }
        }
    }
    out
}

/// 把用户手输的地址补全成 `http://host:port`。
///
/// 在 Kindle 的软键盘上打字很痛苦，所以尽量宽容：下面这些写法**全都接受**，
/// 结果都是 `http://192.168.1.91:8001`：
///
/// ```text
/// 192.168.1.91
/// 192.168.1.91:8001
/// http://192.168.1.91
/// http://192.168.1.91:8001
/// http://192.168.1.91:8001/api/info
/// ```
///
/// 做三件事：补 `http://`、砍掉路径、没写端口就补默认端口。
pub fn normalize_base(input: &str) -> String {
    let t = input.trim();
    if t.is_empty() {
        return String::new();
    }

    let with_scheme = if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("http://{t}")
    };

    // 去掉路径：只在 host[:port] 之后出现的第一个 '/' 处截断
    let after_scheme = with_scheme.find("://").map(|i| i + 3).unwrap_or(0);
    let hostport = match with_scheme[after_scheme..].find('/') {
        Some(i) => with_scheme[..after_scheme + i].to_string(),
        None => with_scheme,
    };

    // 没写端口就补上，省得用户还要记 8001
    if hostport[after_scheme..].contains(':') {
        hostport
    } else {
        format!("{hostport}:{DEFAULT_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 地址补全() {
        // 各种偷懒写法都该归一到同一个结果
        for input in [
            "192.168.1.91",
            "192.168.1.91:8001",
            "http://192.168.1.91",
            "http://192.168.1.91:8001",
            "http://192.168.1.91:8001/api/info",
            " 192.168.1.91 ",
        ] {
            assert_eq!(
                normalize_base(input),
                "http://192.168.1.91:8001",
                "输入 {input:?} 没归一"
            );
        }

        // 显式写了别的端口要尊重
        assert_eq!(normalize_base("10.0.0.5:9000"), "http://10.0.0.5:9000");
        // 空输入
        assert_eq!(normalize_base("   "), "");
    }

    #[test]
    fn 百分号编码() {
        // 中日文要按 UTF-8 逐字节转义
        assert_eq!(urlencode("abc"), "abc");
        assert_eq!(urlencode("中"), "%E4%B8%AD");
        assert_eq!(urlencode("a b"), "a%20b");
        // unreserved 字符不该被转义
        assert_eq!(urlencode("-_.~"), "-_.~");
    }
}

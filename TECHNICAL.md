# 技术细节

> 想了解怎么用，看 [README.md](README.md)。这份写给想改代码 / 想搞明白为什么不那样做的人。

## 目录

- [架构](#架构)
- [代码结构](#代码结构)
- [E Ink 后端（vendor 的 fork）](#e-ink-后端vendor-的-fork)
- [图像处理](#图像处理)
- [线程模型](#线程模型)
- [图片解密](#图片解密)
- [界面约束](#界面约束)
- [构建](#构建)
- [测试](#测试)
- [模拟器](#模拟器)
- [踩过的坑](#踩过的坑)
- [已知限制](#已知限制)

---

## 架构

```
┌────────────────────────── Kindle ──────────────────────────┐
│                                                            │
│   Go 服务端 (127.0.0.1:8001)        Rust 客户端 (Slint)     │
│   ┌────────────────────┐            ┌──────────────────┐   │
│   │ 1. 下载原图         │            │ 1. 请求 /img/...  │   │
│   │ 2. 原样存盘         │  ──HTTP──► │ 2. 解码 webp      │   │
│   │ 3. 原样发出去       │            │ 3. 解密(横条重排) │   │
│   │                    │            │ 4. 缩放适配屏幕   │   │
│   │ 不做任何图片处理    │            │ 5. 显示           │   │
│   └────────────────────┘            └──────────────────┘   │
│                                                            │
└────────────────────────────────────────────────────────────┘
```

**服务端全程不碰像素**——零解码、零编码、零缩放。

这么设计是为了绕开 Go 生态的两个硬伤：没有 webp 编码器、webp 解码有色偏。
既然服务端根本不解码，这两个问题就不存在了。而客户端本来就要解码和缩放，
只是把「解密」这份活从服务端拿回来（多几毫秒）。

另一个好处：缓存在 Kindle 本地（`/mnt/us/kindle-manga-cache`），
单页 200~350 KB，6 GB 可用空间大约能存 800 章。

服务端接口契约在 [API-CONTRACT.md](API-CONTRACT.md)。也可以继续用电脑上的
Python 服务端，客户端一行不用改。

---

## 代码结构

```
src/
├── main.rs        前台：状态机、命令分发、异步请求的身份校验（最大，2.4 万行）
├── render.rs      解码 / 重采样 / 长条漫切片 / 屏保抖动
├── api.rs         手写的 HTTP 客户端 + JSON 模型（不依赖 tokio / ureq）
├── ime.rs         拼音输入法：二分查表 + 分词回退
├── descramble.rs  禁漫横条重排解密
├── md5.rs         MD5（只为算分组数，不到 100 行）
├── favorites.rs   收藏（本地文件 + 可替换的 Backend trait）
├── recent.rs      最近搜索（本地文件）
├── power.rs       电源键 / 合盖休眠（直接读 evdev）
└── screensaver.rs 挂起前铺屏保

ui/
├── tokens.slint   六档灰、描边、字号、骨架高度
├── icons.slint    矢量图标（Path 吃 SVG 数据）
├── chrome.slint   底栏 / 状态条 / 顶栏 / 翻页条 / 列表行 / 封面格
└── app.slint      16 个屏

server/
├── main.go        路由、/api/info、启停
├── handlers.go    search / album / photo / img / cover / history
├── search.go      上游 80 条/页 → 显示 6 条/页
├── cache.go       磁盘缓存 + 原子写 + 上限淘汰
├── history.go     观看记录
├── seg.go         分组数计算
└── jmapi/         打了 3 个补丁的上游库

vendor/
└── slint-backend-kindle/   上游 0.7.1 + 全屏刷新补丁
```

---

## E Ink 后端（vendor 的 fork）

`vendor/slint-backend-kindle/` 是上游 0.7.1 的副本，唯一的功能性改动是新增
`src/refresh.rs`，并在 `lib.rs` / `platform.rs` 里接线。

**为什么必须 fork**：上游把 `Framebuffer` 和 `refresh_full()` 都设成了
`pub(crate)`，应用层拿不到「立即全屏刷新」的入口。而墨水残影只能靠周期性
全刷清掉，这是本项目的核心需求之一。

**刷新策略**：用全刷「代替」那一帧的局部刷，不是刷两遍。

```
每 N 次刷新 → 第 N 次走 refresh_full，其余走 refresh_region
```

实现见 `RefreshPolicy`。进出阅读器时切换一次策略（不是每帧切），
离开阅读器会补一次全刷清掉漫画残影。

升级上游时的重新套用步骤见 `vendor/slint-backend-kindle/PATCH.md`。

---

## 图像处理

### 重采样：自己写，比 `image` crate 快 7~9 倍

把 1280×1818 的漫画页缩到 1160×1648，实测（x86 release）：

| 实现 | 单次耗时 |
|---|---|
| **本项目** | **45 ms** |
| `image` · Triangle | 305 ms |
| `image` · CatmullRom | 332 ms |
| `image` · Lanczos3 | 416 ms |

算法本身不新鲜，快在工程细节：

- **缩小**用面积平均（box filter）：每个目标像素 = 它覆盖的那段源像素的加权平均
- **放大**用双线性
- 定点整数：每轴 10 位权重，两轴合成 20 位，u32 累加不溢出。ARM 上无浮点
- **两维一趟算完**，不落中间缓冲（分两趟会多读写 8.4 MB）
- alpha 不参与加权（漫画页全不透明），省 25% 乘法

### 为什么不能用最近邻

原来就是最近邻，用户反馈「图有点糊」。同一张图对比（缩放 0.906）：

| 缩放方式 | 与 Lanczos 的平均绝对差 | 尖锐度 |
|---|---|---|
| 最近邻 | **2.40** | 375.7 |
| 三角滤波 | 1.26 | 233.7 |
| **面积平均（现在）** | **1.16** | — |
| Lanczos（基准） | — | 348.4 |

> ⚠️ **最近邻的「尖锐度」反而最高**——那不是清晰，是锯齿伪高频。
> 在墨水屏上看着就是边缘发脏。别拿「更锐 = 更好」判断。

### 解码内存预算

分两档，因为长条漫和普通页的需求完全不同：

| 类型 | 上限 | 理由 |
|---|---|---|
| 普通页 | 2.5 M 像素 | 屏幕只有 2.04 M 像素，存更多是浪费。每张 RGBA 最多约 10 MB |
| 长条漫（高/宽 > 2） | 8 M 像素 | 长条漫按宽度缩放，宽度就是清晰度。压到 2.5 M 会缩成 386 宽，糊得没法看 |

超出预算在**解码阶段**就降采样，不会让 24.7 M 像素的图占满内存。

### 长条漫切片

长条漫不整页缩放，而是只按宽度适配 + 纵向切屏。

⚠️ 切屏时**必须把目标尺寸夹到画布内**——`step = round(VIEW_H / scale)` 四舍五入
可能向上，`dst_h = round(take × scale)` 因此会比画布高 1~2 行，而拷贝是不带越界
保护的。真机上崩过两次（`range end index 8147716 out of range for slice of length
8147712`，8147712 正是 1236×1648×4）。200~1600 之间有 42 个图宽档会触发。

回归测试 `长条漫切屏不越界` 遍历 23 个宽度档。

---

## 线程模型

```
UI 线程      只投递意图（Cmd），不做任何阻塞动作
worker 线程  独占状态，只处理 Cmd，**不做任何网络 I/O**
临时线程     每个请求一个，做完 tx.send(Cmd::XxxDone{...}) 送回 worker
```

`tx` / `rx` 是同一条 channel：worker 自己持有一个 `Sender`，后台线程用它把结果
送回来，worker 像处理普通点击那样处理。不需要额外的同步原语。

### 身份校验（异步化的必需代价）

原来所有请求同步跑在 worker 上，于是「加载那几秒里点什么都没反应，加载完
一次性全执行」。改成全异步之后，**每个请求都要带身份**，回来时对不上就丢弃：

| 消息 | 身份 |
|---|---|
| `BootStatus` / `BootDone` | `seq` |
| `SearchDone` | `(关键词, 排序, 页码, 发起时在哪一屏)` |
| `HistoryDone` | 屏（必须还在历史页） |
| `AlbumDone` / `AlbumChapterDone` | `(album_id, 屏)` |
| `PhotoDone` | `photo_id` |
| `PageReady` | `(章节, 页码, 适屏方式)` |

⚠️ **搜索的身份里必须带「发起时在哪一屏」**，不能简单判「还在不在搜索页」——
标签搜索是从详情页发起的，靠结果回来才切到搜索页，用「还在搜索页」当判据
会把这条路径整个打死。

不校验的后果：晚回来的结果会把用户**从当前页面硬拽走**，或者覆盖共用的 `rows`。
`mod async_guard_tests` 有 13 条测试钉住这两类错误。

---

## 图片解密

来源站把每页按横条切乱。下面和 jmcomic 的 `JmImageTool.get_num` 逐字对齐：

```
aid < scramble_id          → 0（这本不加密）
aid < 268850               → 10
否则: x = (aid < 421926) ? 10 : 8
      s = md5("{aid}{原始文件名}")     # 文件名不含后缀，如 00001
      num = (ord(s[-1]) % x) * 2 + 2
```

`aid` 是**章节 ID**，不是本子 ID。

⚠️ 最后一步的 `ord` 是**十六进制字符串里那个字符的 ASCII 码**，不是哈希字节本身。
`'0'..'9'` 是 48~57，`'a'..'f'` 是 97~102。

### 分段数逐页不同

实测同一章 32 页里出现了 2/4/6/8/10/12/14 六种值。而客户端拿到的地址是
`/img/<photo_id>/<page>`，**看不到原始文件名**——所以服务端要么把 `seg` 直接
算好，要么把 `filename` 一起发过来。契约见 [API-CONTRACT.md](API-CONTRACT.md)。

### 兼容旧服务端

**没有 `scramble_id` 字段 = 不解密。** 旧版 Python 服务端发的是已经解好密的图。
删掉这条判断的话，换回旧服务端会把好图再切一遍。

### 验证

| 项目 | 结果 |
|---|---|
| 算法向量 | 12 组 `(scramble_id, aid, 文件名) → 分段数`，与 jmcomic 逐条比对 |
| 真实图片 | 原图 + 客户端解密 vs jmcomic 正常解密：**MAD = 1.214**（阈值 3） |
| 真的作用到画布 | 造渐变图走完整 `finish_decode`，比对输出方向 |

真实样本测试默认 `#[ignore]`（样本有版权，不在仓库里）。

---

## 缓存上限与清理

服务端只缓存**源站原图**，目录是 `/mnt/us/kindle-manga-cache/<photo_id>/0001.webp`。

上限存在客户端的 `cache_limit.txt`（程序目录，单位 MB，`0` = 不限制），有两个读者：

| 谁读 | 什么时候 | 干什么 |
|---|---|---|
| `launch.sh` | 启动服务端前 | 作为 `-cache-max-mb` 传给 Go 进程 |
| 客户端 | 连上服务端后 / 用户改值时 | `POST /api/cache {max_mb}` 推给服务端 |

为什么要推两次：`launch.sh` 只在启动那一刻读一次文件，而用户可以在运行中改值 ——
已经跑着的进程不知道，必须走接口通知它。

### 淘汰策略

超上限时按 **mtime 从旧到新删**，直到回到上限以内。

- FAT32 上 atime 不可靠（挂载时常常是 `noatime`/`relatime`），所以用 mtime
  （≈ 下载时间）。顺序上「最近下载的」和「最近看的」基本一致。
- `prune()` 有 10 分钟节流：每次写缓存都全盘扫一遍在 Kindle 上太慢。
  但 `SetMaxMB()` 会**清掉节流时间戳**，保证用户调小上限时立刻清理。
- 启动时 `recount()` 先数一遍实际用量。不数的话界面上的「已用」会一直是 0，
  直到第一次 prune 才变成真值（这个坑在桌面版上很明显）。

### 原子写

先写 `.part` 再 `rename`。Kindle 是 FAT32，直接覆写时断电会留下半截 webp，
而半截文件会让客户端解码失败 —— 表现是「这一页永远加载不出来」。
`.part` 文件不计入用量、也不当缓存命中。

---

## 加载圈（为什么不用旋转）

右下角那个转圈是**预生成的 12 段圆弧**，按 `phase` 选一段，不是旋转一个元素。

原因很硬：

```
i-slint-renderer-software-1.18.1/lib.rs:3234
    const SUPPORTS_TRANSFORMATIONS: bool = false;
```

软件渲染器**不支持元素变换**，而真机（Kindle）走的就是它。给元素加
`rotation-angle` 在桌面上可能碰巧看着对，到真机上就不转了 —— 这类
「桌面能跑、真机不行」的坑在这个项目里出现过好几次。

所以 `ui/chrome.slint` 里的 `ArcPaths` 是 12 条写死的 SVG 弧线，由脚本生成：

```
for p in range(12):
    a0 = -90 + p*30; a1 = a0 + 270          # 270° 弧，留 90° 缺口
    "M {x(a0)} {y(a0)} A 17 17 0 1 1 {x(a1)} {y(a1)}"
```

形状照用户给的设计稿：`r=2.25 / stroke=0.5`（描边占直径 11%）。这里放大到
44px 盒子、r=17、stroke 6px（17.6%，比稿子更厚一点）。

两个容易踩的点：

- **不能靠 `fill` 把圈外面涂掉**：路径是开口弧，填充会隐式闭合出饼形。
  必须单独画一个白色圆当底（顺带解决「圈盖在漫画上看不清」）。
- **`fill` 必须显式给**：不写或写 `transparent` 在这块屏上会渲染成纯黑，
  整个圆变成实心黑饼。实测暗像素会从 ~95 跳到 ~380。

### ⚠️ 转圈不能触发整屏全刷（踩过）

第一版上线后真机上「转圈时整个屏幕一直闪黑白」。根因是三处串起来的：

```
转圈每 250ms 重绘一次
  → RefreshPolicy 数的是「重绘次数」，每 N 次（真机 N=2）全刷一次
  → 500ms 一次整屏黑→白
```

圈只有 44×44（面板的 0.1%），根本不积累残影。修法在后端：
`RefreshPolicy::wants_full_refresh(dirty, panel)` 增加面积参数，
**小于面板 2% 的更新不计数、不触发全刷**。

| 变化 | 面积占比 |
|---|---|
| 加载圈 44×44 | 0.1% |
| 列表一行 | ~5% |
| 整页翻页 | 100% |

阈值 2% 卡在「小控件」和「真的换了内容」之间。`force_next_refresh()`
（用户改完设置要立刻看到效果）**不受**面积判断影响。

> 这个 bug 不在应用层：`main.rs` 里「loading → 每 250ms emit」是对的，
> 错的是后端把 0.1% 面积的重绘当成「该清残影的一页」。应用层也无从知道
> 自己那次 emit 会不会触发整屏全刷 —— 所以必须修在后端。

### 步进速度

`SPIN_INTERVAL_MS = 250`，12 格 = **3 秒一圈**，和用户设计稿的
`animation: spin 3s linear infinite` 一致。

驱动方式是 worker 里的一个后台线程，每 250ms 发一条 `Cmd::SpinTick`：

- 不在加载时直接 `return`，**连 emit 都不发** —— 否则空闲状态每 250ms
  白重画一次，墨水屏上就是持续白闪
- 线程的启停跟着 `loading`，由 `emit()` 统一调用 `sync_spin()`。
  放在 emit 里是因为它是唯一的状态出口，分散到各个「把 loading 关掉」的
  地方迟早会漏一个

---

## 详情页的标签区（角色 + 标签）

详情页有两组可点的筛选标签：**角色**在前、**标签**在后。位置在 Rust 里算好
（`layout_tag_groups`），Slint 只按坐标画 —— 因为 Slint 没有内置的换行流式布局。

### 为什么标题要单独一个 model

```slint
in property <[TagChip]> album-tags: [];     // 可点的标签
in property <[TagHeader]> tag-headers: [];  // 不可点的小标题
```

混在一个 model 里的话，`tag-activated(idx)` 的索引会因为中间插了几个标题
而整体偏移 —— 表现是「点第 3 个标签，搜出来的是第 5 个」。
分成两个模型，索引天然对齐。

对应地，`tag_texts` **从排好版的 chips 里取**，不另存一份原数组：

```rust
self.tag_texts = chips.iter().map(|c| c.label.clone()).collect();
```

另存一份的话，排版截断之后两边长度不同，索引又开始对不上。

### 版式

标签区从 `y=496` 开始，最多 3 行（`MAX_ROWS`），每行 66px + 14px 间距：

```
496 ─┬─ 角色  [森亚露露卡]
     │  ...
576 ─┼─ ...
     │
656 ─┼─ ...
722 ─┴─  ← 标签区底
744      目录标题
800      章节第一行（6 行 × 100px → 1400，正好停在翻页条）
```

左边 84px 是标题列，标签从它右边开始排。

### 行数分配：先到先得

3 行的预算是**角色先占、标签用剩下的**：

- 实测最多的一个本子有 11 个角色 → 占 2 行，标签只剩 1 行
- 角色多到占满 3 行时，标签**整组不显示**（而不是画一个没内容的标题）

这是刻意的：角色是用户明确要求加的，而且数量有限；标签通常十几个，
截断几个影响不大。

### 没加「作品」

上游还有 `works`（原作/系列，如「名侦探光之美少女」），服务端已经返回，
但客户端**不显示**：三个组会占满 3 行预算，标签就只剩 1 行，12 个标签的
常见本子会被截掉一大半。作品名基本已经写在标题里了。

要加的话把 `apply_album_tags` 里的 `groups` 加一项即可，服务端不用改。

---

## 界面约束

16 屏，底栏五项常驻（历史 / 搜索 / 主页 / 收藏 / 设置），首屏是历史记录。
阅读器真·全屏，没有任何栏。

设计规格在 `design/UI-SPEC.md`，1:1 稿是 `design/ui-preview.html`。

几条硬约束（**违反了不报错，只是看着不对**）：

| 约束 | 为什么 |
|---|---|
| **只用六档灰** | 面板只有 16 级灰，且 238~255 之间是空的。「很浅的灰」做不出来，会变成深浅不匀的色带 |
| 描边至少 2px | 1px 的线会被采样吃成断线，看起来像没画 |
| 图标一律矢量 `Path` | `✕ ▶ ⌫ ★` 在文泉驿里**掉成空白**，而且不报错 |
| 每屏必须有底栏（阅读器除外） | 少了用户会以为程序坏了 |
| 出错页必须自带退出入口 | 断网时到不了主页，否则只能长按电源键重启 |
| 没有动画 / 过渡 / 定时器 | 墨水屏上会留一串残影 |

`check-ui-spec.sh` 把能机器查的都查了（18 条）。

### 光标为什么是独立格子

输入框里的光标**不能自己算 x**。原来的 `caret_x()` 按「中日文 44 / ASCII 28」
估宽度，实际渲染步进约 26——**误差随字数累积**，每打一个字就多偏 5px。

现在光标是 `HorizontalLayout` 里紧跟 `Text` 的一个格子，让渲染器自己排，
结构上不可能再漂。

两个附带坑：

- 垂直居中必须用 `cross-axis-alignment`，`alignment` 管的是**水平**轴
- 宽度必须是**偶数**：3px × 桌面缩放 0.5 = 1.5 物理像素，`round()` 会在 1↔2 间摇摆。
  判据是「总墨量只有两种值、没有过渡灰」= 取整而非抗锯齿

---

## 构建

### 客户端

```bash
# 桌面预览（改 UI 用，秒级迭代）
cargo run --release --features desktop

# 真机（静态 musl，交叉编译）
cargo zigbuild --release --features kindle --target armv7-unknown-linux-musleabihf
```

桌面预览**必须**用 `SLINT_SCALE_FACTOR=0.5`——`ui/*.slint` 里是 1236×1648 的
绝对坐标，缩放 0.5 得到逻辑尺寸恰好 1236×1648，和真机排版一致。

交叉编译需要：

- Rust 的 `armv7-unknown-linux-musleabihf` target
- [`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) + zig
- 模拟用的 `armv7-unknown-linux-gnueabihf` 还要 Kindle 官方工具链（见 `ENVIRONMENT.md`）

### 服务端

```bash
cd server
CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=7 \
  go build -trimpath -ldflags "-s -w" -o kindle-manga-server .
```

### 打包成 KUAL 扩展

```bash
bash kual/package-kual.sh
```

它会编译两个二进制（Rust + Go）、组装 `kual/extensions/kindle-manga/`、
校验文件完整性，最后打包成 zip。

`launch.sh` 的启动顺序：

1. 启动 `kindle-manga-server -addr 127.0.0.1:8001`（后台）
2. 轮询就绪文件，最多等 20 秒
3. 暂停 Kindle 框架（`-STOP`，不是 kill），跑客户端
4. **EXIT trap 里先杀服务端，再恢复框架**

⚠️ 步骤 4 的顺序很重要：服务端还在下载会占 WiFi 和 CPU，而接下来要整屏重绘。

### 关于地址优先级

客户端选服务端的顺序：`--server` 参数 > `server.txt`（用户在设置里填的）>
`MANGA_SERVER` 环境变量。

`launch.sh` 把内置地址放进**最低优先级**的 `MANGA_SERVER`：

| 用户情况 | 实际用哪个 |
|---|---|
| 没填过地址 | 内置服务端（开箱即用） |
| 填了电脑 IP | 电脑（`server.txt`） |
| 填了但连不上 | 探活失败后自动落到内置 |

---

## 测试

```bash
cargo test --workspace          # 客户端 136 + vendor 后端 11
cd server && go test ./...      # 服务端
bash check-ui-spec.sh           # UI 规格 18 条
bash check-font-coverage.sh     # 字体覆盖（缺字渲染成空白，不报错）
```

### 需要样本的测试（默认忽略）

样本有版权，不在仓库里。准备两份放到 `samples/`：

- `raw_00.webp` —— 从来源站下载的**未解密**原图
- `py_decoded_0.webp` —— 用 jmcomic 正常解密的结果

```bash
cargo test --release --bin kindle-manga 真实样本 -- --ignored --nocapture
cargo test --release --bin kindle-manga 缩放质量 -- --ignored --nocapture
```

### 写测试的几条经验（这个项目特有）

- **`cargo check` 不产出二进制**，增量判断还会「空转」→ 用 `_force_build.sh`
- **测之前必须重编**：直接用旧二进制会「改了没效果」（栽过两次）
- **写 grep 类检查必须先剥注释**（`sed 's|//.*||'`）：
  说明文字里写着的字样会把检查喂饱 = **假绿灯**
- **每加一条 grep 检查都要做负向验证**（故意改坏，看它报不报错）

---

## 模拟器

`emulator/` 是一套 QEMU 环境，用来在没有真机的情况下跑完整交互测试。

自写的三个 shim 通过 `LD_PRELOAD` 注入：

| 文件 | 作用 |
|---|---|
| `fbshim.c` | 把 `/dev/fb0` 接到宿主机 |
| `touchshim.c` | 注入触摸事件 |
| `lfscompat.c` | 兼容层文件系统 |

```bash
bash emulator/build-emulator.sh
bash emulator/verify-all.sh
```

> ⚠️ 模拟环境**必须**用动态链接的 `gnueabihf` 目标构建——静态二进制没有
> 动态链接器，`LD_PRELOAD` 注入不进去。

---

## 踩过的坑

完整的记录在 [BUGFIX.md](BUGFIX.md)。这里只列几条最容易重犯的。

### 多个屏共用一个字段

**栽过三次**：`nav_return`、`rows`、`col_left/col_right`。

判据：一个 `rebuild_*` 里出现 `self.xxx.clear()`，或者给**这一屏根本不读**的字段
赋值——那基本就是 bug。

异步化之后这条以新形式重现：晚回来的结果覆盖当前屏的 `rows`。

### `sync.Mutex` 不可重入

```go
func (h *History) Put(...) {
    h.mu.Lock()
    defer h.mu.Unlock()
    ...
    return h.List()    // ← List() 里又 Lock 一次，永久死锁
}
```

表现是请求挂住，锁一直被占着，之后所有相关请求全堵死。**日志里看不出来。**

修法是拆一个不带锁的 `listLocked()` 给已持锁的内部方法用。测试给每个操作
套了 5 秒超时——死锁的话测试会**失败**而不是挂住。

### 服务端在监听之前同步初始化

`jmapi.NewClient(AutoUpdateHost: true)` 要更新域名列表（正常 0.45 秒，网络差时
几十秒）。它排在 `net.Listen` 前面的话，那段时间端口是关的——客户端连上来直接
`Connection refused`，表现是「整个应用打不开」。

改成：**先监听 → 后台构造上游 → 需要数据的接口等就绪**。

### `pidof` 找不到长进程名

Linux 的 `/proc/PID/comm` 只有 **15 个字符**，`kindle-manga-server` 被截成
`kindle-manga-se`，`pidof kindle-manga-server` 永远找不到。

改用就绪文件里记的 PID 再 `kill -0` 验证。

### 前缀判断不能靠 `lastSegment` 判空

```go
// 想区分 DELETE /api/history 和 DELETE /api/history/123
lastSegment(strings.TrimPrefix(path, "/api/history/"))
```

`TrimPrefix` 前缀不匹配时**原样返回**，于是 `lastSegment("/api/history")` 得到
`"history"`——非空！「清空全部」被当成「删掉 id 为 history 的那条」，
**返回 200 但一条没少**。

---

## 已知限制

- 只在 **Kindle Paperwhite 5 + 系统版本 5.19.2**
  （`039-juno_1902_malbec_bellatrix-432036`）上验证过。
  其他机型 / 固件的面板尺寸、EPDC ioctl 变体可能不同（后端里有探测逻辑，
  但没有设备可测）
- 服务端依赖第三方站点的接口，**它们改接口就会失效**
- 只用 Linux 内核的接口（`/dev/fb0`、`/dev/input/event*`、`lipc-set-prop`），
  没有其他平台支持
- 翻回来会重新解码 + 重新缩放，没有做「渲染结果缓存」
- 搜索、目录这些列表都是**分页**的，没有滚动

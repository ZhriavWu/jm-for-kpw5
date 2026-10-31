# Kindle 原生开发环境

给 **Kindle Paperwhite 5（11 代，2021）** 做原生应用（Rust / C / C++）的完整开发环境，
运行在 Windows 上的 WSL2 里。

包含三样东西：

1. **交叉编译工具链** —— 产出能跑在 Kindle 上的 ARM 二进制
2. **全系统模拟器** —— QEMU 里跑真实的 Kindle 系统，**不需要真机就能演示**
3. **KUAL 打包脚本** —— 打成能直接拖进 Kindle 的扩展

---

## 目录

- [环境现状](#环境现状)
- [目标设备事实](#目标设备事实)
- [四种运行模式](#四种运行模式)
- [常用命令](#常用命令)
- [怎么接入你自己的客户端](#怎么接入你自己的客户端)
- [踩过的坑（重要）](#踩过的坑重要)
- [没验证的部分](#没验证的部分)

---

## 环境现状

### 位置一览

| 东西 | 路径 |
|---|---|
| WSL 发行版虚拟盘 | `D:\WSL\distro\ext4.vhdx`（装在 D 盘，因为 C 盘只剩 10.9 GB） |
| 交叉编译工具链 | `/root/x-tools/arm-kindlehf-linux-gnueabihf/` |
| 工具链实体 | `/opt/kindle-dev/xt/x-tools/arm-kindlehf-linux-gnueabihf/` |
| 从固件解出的 rootfs | `/opt/kindle-dev/fw/rootfs.img`（ext3，512 MB） |
| Debian 内核 + 设备树 | `/opt/kindle-dev/qemu/armmp/` |
| 模拟器产物 | `/opt/kindle-dev/emulator/out/` |
| 编译缓存 | `/root/target/kindle-manga/` |
| 项目本体 | `.`（即 `.`） |

`CARGO_TARGET_DIR` 故意设在 WSL 内部的 `/root/target`，**不是** E 盘 ——
WSL 访问 `/mnt/e` 走 9p，编译会明显变慢。

### 组件与版本

| 组件 | 版本 | 说明 |
|---|---|---|
| Ubuntu | 24.04 LTS (noble) | WSL2，`wsl -d Ubuntu`，默认 root |
| 内核 | 6.6.87.2-microsoft-standard-WSL2 | |
| 交叉编译器 | GCC 14.4.0 | `arm-kindlehf-linux-gnueabihf-`，crosstool-NG by NiLuJe |
| Rust | 1.99.0 | |
| cargo-zigbuild | 0.23.4 | 配 zig 0.16.0，用于产出静态 musl 二进制 |
| KindleTool | v1.6.5 | 解包/打包 Amazon 的 `.bin` 固件 |
| kff | 0.1.1 | Kindle 开发环境管理器 |
| QEMU | 8.2.2 | `qemu-system-arm` |
| 模拟用内核 | Debian 6.12.94+deb13-armmp | vexpress-a9 平台 |

### 网络

WSL 内**直连可用**，不需要代理：

- `github.com`、`codeload.github.com`、`static.crates.io` ✓
- `raw.githubusercontent.com` ✗（DNS 被污染成 `0.0.0.0`）→ 用 `ghfast.top` 前缀绕过
- apt 源已换成清华，cargo 源已换成清华 sparse registry，都在 `~/.cargo/config.toml`

GitHub 直连很慢（约 40 KB/s），走 `https://ghfast.top/https://github.com/...` 能到 5 MB/s。

### 验证状态

跑 `emulator/verify-all.sh`，当前 **9 项全通过**：

```
1/4 构建        build-emulator.sh 退出码 0；kindle-manga / initrd.img / rootfs-emu.img 都在
2/4 渲染链路    应用 → fbshim → socket，收到帧且画面有内容
3/4 触摸翻页    点击真的换页；无重连风暴
4/4 网络        guest 通过 HTTP 从宿主取到 JPEG（GET /img/0/0 → 200）
```

---

## 目标设备事实

这些都是从你的固件里实测出来的，不是猜的。**做开发决策时以这些为准。**

| 项目 | 值 | 怎么知道的 |
|---|---|---|
| 机型 | Paperwhite 5（11 代，2021），9 个变体 | `kindletool extract` 的 Bundle 信息 |
| SoC 平台 | **MediaTek MT8110「Bellatrix」**，不是 i.MX | 固件内有 `mt8110_bellatrix/u-boot.bin`、`mediatek/hwtcon.ko`、`wmt_*` 无线驱动 |
| 固件版本 | 5.19.2（build `040-juno_1902_malbec_bellatrix-467076`） | `/etc/version.txt` |
| 内核 | 4.9.77-lab126 | `/lib/modules/` |
| C 库 | **glibc 2.20**（硬浮点，`/lib/ld-linux-armhf.so.3`） | rootfs |
| 屏幕 | **1236 × 1648** | 面板规格，与 `fbshim.c` 对齐 |
| 显示驱动 | `hwtcon.ko`，**但实现了 MXCFB ioctl** | 反汇编见下 |
| 触摸 | `mediatek/gt9xx`，协议 B 多点触控（`ABS_MT_POSITION_X` = 0x35） | rootfs 模块 + 后端探测逻辑 |

### 关于显示的坑（这条最要紧）

PW5 是 **MediaTek** 平台，`hwtcon.ko` 里出现的却是 i.MX 的名字：

```
[HWTCON EPDC][%d] Sending update. waveform:%d mode:0x%d update region top=%d...
mxc_epdc_fb:SW mitigation count=%d strength=%d
```

反汇编 `hwtcon_fb_ioctl` 拿到实锤：

```
a84:  movw  r2, #0x462e
a88:  movt  r2, #0x4060      →  0x4060462e
a8c:  cmp   r1, r2
a98:  bl    hwtcon_fb_ioctl_send_update
```

`0x4060462e` 正是 MXCFB 的 `SEND_UPDATE`（`'F'`, `0x2e`），结构体 96 字节 ——
对应 `slint-backend-kindle` 里的 `MXCFB_SEND_UPDATE_MTK`。

**结论：Amazon 把 i.MX 的 mxc_epdc_fb 移植到了 MTK 硬件上，保留了 MXCFB 的 ioctl ABI。**
所以任何按 MXCFB 协议写的 Kindle 应用在 PW5 上都能刷新屏幕。

### 你没越狱/没装 KUAL 的话

应用无法运行。`I:\` 里已经有 `Update_usbnetlite_*_install_khf_11thgenplus.bin`，
说明这条路已经开了个头，但**真机当前状态没有验证过**。

---

## 四种运行模式

### 构建目标（三选一）

| 用途 | 目标三元组 | 链接方式 | 为什么 |
|---|---|---|---|
| **桌面预览** | `x86_64-unknown-linux-gnu` | 动态 | 秒级迭代 UI，不用碰 QEMU |
| **模拟器** | `armv7-unknown-linux-gnueabihf` | **动态** | 必须动态，静态二进制注入不了 `LD_PRELOAD` shim |
| **真机部署** | `armv7-unknown-linux-musleabihf` | **静态** | 不依赖 Kindle 的库版本，7 MB 单文件 |

动态版（`gnueabihf`）的符号需求只到 `GLIBC_2.18`，而 Kindle 是 2.20，
**所以动态版其实也能直接部署到真机**，多一个选择。

### 模拟器内的测试模式

由内核参数 `kdev.mode=` 控制：

| 模式 | 干什么 |
|---|---|
| `demo`（默认） | 本地生成 PPM 测试图，验证渲染与触摸 |
| `dir` | 从 `/mnt/us/emu/pages` 读 JPEG，验证 JPEG 解码与多页翻页 |
| `remote` | 走 HTTP 从宿主取图（**真机的生产路径**） |
| `kual` | 走 KUAL 的 `launch.sh`，验证真机那条启动脚本 |
| `net` | 纯网络自检，不启动应用 |

---

## 常用命令

**全部在 WSL 里执行**，先 `wsl -d Ubuntu`。

### 环境自检（先跑这个）

```bash
bash ./emulator/verify-all.sh
```

### 构建 + 启动模拟器（带窗口）

```bash
# 1. 构建：编译 shim、编译应用、重做 rootfs 副本和 initramfs
bash ./emulator/build-emulator.sh

# 2. 启动（弹窗口，鼠标点击 = 触摸，左/右两侧 = 翻页）
bash ./emulator/run-emulator.sh
```

`run-emulator.sh` 的开关：

```bash
run-emulator.sh --shell   # 不启动应用，直接进 Kindle 的 shell 调试
run-emulator.sh --logs    # 跟随串口日志
```

### 无头测试（看不到 GUI 时用）

```bash
bash emulator/e2e-test.sh 35 demo      # 渲染链路
bash emulator/interact-test.sh 30      # 触摸翻页
bash emulator/pagewalk-test.sh 4       # 多页翻页 + 边界
bash emulator/jpeg-test.sh 25          # JPEG 解码路径
bash emulator/net-test.sh 8000         # 网络（会起一个 mock 服务）
bash emulator/kual-test.sh 25          # KUAL launch.sh
```

### 打包真机扩展

```bash
bash ./kual/package-kual.sh
```

产出 `kual/extensions/kindle-manga/`（可直接拖进 Kindle）和 `kual/kindle-manga-0.1.0.zip`。

### 手动交叉编译

```bash
. /root/.cargo/env
export CARGO_TARGET_DIR=/root/target/kindle-manga

# 真机（静态）
cargo zigbuild --release --target armv7-unknown-linux-musleabihf

# 模拟器（动态，必须用 kindlehf 工具链当链接器，已在 .cargo/config.toml 配好）
export PATH="$HOME/x-tools/arm-kindlehf-linux-gnueabihf/bin:$PATH"
cargo build --release --target armv7-unknown-linux-gnueabihf
```

### 部署到真机（两种方式）

**A. USB 拖拽**（用户装法，不需要 SSH）

1. USB 连 Kindle
2. 把 `kual/extensions/kindle-manga` 整个文件夹拖进 **`Kindle 根目录/extensions/`**
3. 弹出设备 → Kindle 上打开 KUAL → 点「启动漫画阅读器」

**B. SSH**（开发调试，需先装 USBNetLite）

```bash
scp -r kual/extensions/kindle-manga root@192.168.15.244:/mnt/us/extensions/
```

---

## 怎么接入你自己的客户端

### 现在的 `src/` 和 `ui/` 是占位夹具，不是成品

`src/main.rs`、`ui/app.slint`、`emulator/mock-server.py` 是为了验证环境而编的
**测试夹具**。它们证明了「ARM 二进制 → 帧缓冲 → 屏幕」「触摸 → 事件」「HTTP → 解码」
三条链路都能通。**你的真实客户端代码还没有，环境不依赖它。**

替换方式：把 `src/` 换掉，保持下面几点即可。

### 交叉编译时注意

1. **用 `--features kindle`**（或你自己的等价开关）切到 E Ink 后端。
   官方 `slint-backend-kindle` 是按 `target_env = "musl"` 判断的，但那样只有静态版
   能走 E Ink，模拟器就没法用同一个二进制 —— 所以本项目的 `Cargo.toml` 改用
   **feature 门控**，让模拟器和真机共用一份源码。

2. **字体必须打包进去**。Kindle 没有 fontconfig、没有默认字体路径，
   Slint 的软件渲染器第一次字体回退查询就会 panic。

3. **跨线程传图像要传 `SharedPixelBuffer`，不能传 `slint::Image`** ——
   后者不是 `Send`，塞不进 `invoke_from_event_loop` 的闭包。

4. **HTTP 必须放独立线程**，结果用 `slint::invoke_from_event_loop` 回 UI 线程，
   否则阻塞事件循环。

### 服务端接口（从 `jm_kindle_server.py` 观察到的）

```
GET /                    存活页
GET /search              搜索
GET /album/<id>          目录
GET /read/<id>           阅读页
GET /img/<photo_id>/<page>   取第 page 页图片（JPEG）
GET /cover/<id>          封面
```

客户端只需要 `/img/<photo_id>/<page>`。**翻到末尾时服务端返回 404**，
客户端据此判断「没有下一页」。

**顺带一个待改的 bug**：`jm_kindle_server.py` 里

```python
SCREEN_W = 1072
SCREEN_H = 1448   # 注释写着 PW3/4
```

但你的机器是 **1236 × 1648**，图片会按错误尺寸预缩放。改成 PW5 的尺寸。

---

## 踩过的坑（重要）

这些都是实测踩出来的，改代码前先看一遍能省很多时间。

### Kindle 系统侧

1. **Kindle 的 busybox 被 Amazon 裁过**：326 个 applet 里**没有 `mount`、`modprobe`、
   `insmod`**。必须用独立二进制 `/bin/mount`、`/bin/kmod`（`modprobe`/`insmod` 是它的软链接）。

2. **busybox 的 `sleep` 不接受小数** —— `sleep 0.5` 会报 `invalid number`。
   要等一小会儿就用 `usleep 500000` 或整数 `sleep 1`。

3. **`/tmp` 是指向 `/var/tmp` 的软链接，而 `/var/tmp` 在固件镜像里不存在**
   （真机上由 Kindle 自己的 init 运行时创建）。不预先 `mkdir` 的话，任何往 `/tmp`
   写文件的操作都会失败。

4. **rootfs 是 glibc 2.20**，很老。交叉编译出的二进制符号需求不能超过它。

### 模拟器侧

5. **模块必须在 `switch_root` 之前加载**。切进 Kindle 根文件系统后，
   `/lib/modules` 只有它自己的 `4.9.77-lab126`，找不到 Debian 的 6.12 模块。
   块设备能挂上、网络一开始挂不上，就是这个原因。

6. **网络要三个模块，少一个都不行**：`virtio_net` → `net_failover` → `failover`。
   `modprobe virtio_net` 会因为依赖缺失直接失败。

7. **`virtio_mmio` 不能漏**。vexpress 没有 PCI virtio，QEMU 的 `virtio-blk-device`
   走 MMIO 传输，而 `CONFIG_VIRTIO_MMIO=m`。少了它，`virtio_blk` 能加载但绑不到设备，
   `/dev/vda` 根本不出现。

8. **ext4 需要 `crc32c_generic`**。ext4 的元数据校验通过 crypto API **按名字**查找
   crc32c，不是符号依赖，所以 `modprobe ext4` 不会自动带上它。缺了会报
   `EXT4-fs (vda): Cannot load crc32c driver` 然后挂载失败。

9. **Debian 模块是 `.ko.xz`**，按 `.ko` 拼路径找不到，得按 glob 匹配。

10. **不要用 Debian 的 `mkinitramfs`**：在 x86 主机上为 ARM 内核打包会把主机的
    x86 busybox 塞进去，initrd 里的 `/init` 跑不起来。本项目的 `build-emulator.sh`
    自建 initramfs，全部组件取自 Kindle 固件本身，没有架构错配。

### shim 侧（本项目自己写的部分）

11. **32 位 glibc 上 `open64`/`stat64`/`mmap64` 和 `open`/`stat`/`mmap` 是不同的符号**，
    而 Rust 的 std 在 32 位下走 64 位变体。不转发的话 shim 被整个绕过 ——
    应用打开的是真实 `/dev/fb0`，画面一帧都出不来。
    这就是 `emulator/lfs-compat.c` 存在的原因，它必须排在 `LD_PRELOAD` **最前面**。

12. **`EVIOCGABS` 的 nr 范围是 `0x40..0x7f`，不是 `0x40..0x5f`**。
    `ABS_MT_POSITION_X` = 0x35，所以 nr = 0x75。范围写窄了会静默拒绝坐标范围查询，
    后端回退到 4095，触摸位置全部算到屏幕外。

13. **`close()` 里不能调自己的 `close()`**。shim 拦截了 `close`，在里面再调 `close()`
    会重入同一函数并重锁非递归互斥量 → 自死锁，而且是在持有表锁的情况下死锁，
    之后所有 `open`/`ioctl` 全部卡死。要释放 fd 就用 `real_close()`。

14. **`virtio-serial` 空闲时 `read()` 返回 0，不等于 EOF**。
    把 0 当断开处理会让 pump 线程每个空闲周期都重连一次 —— 重连风暴，还可能丢输入。

15. **`LD_PRELOAD` 对静态二进制完全无效**。所以模拟环境必须用动态链接版本，
    这就是为什么需要 `armv7-unknown-linux-gnueabihf` 这个额外目标。

### 工具链侧

16. **`kff install sdk kindlehf` 没装成**（报 408）。`KSDK` 至今 `[NOT SET]`，
    `kff doctor` 不过。**不影响 Rust 路线** —— SDK 是给 Meson/C 工程提供 pkg-config 和库的。
    要用 C 的话得补跑 `gen-sdk.sh kindlehf`。

17. **`i:\` 里的固件解包不需要序列号**：`kindletool extract <bin> <outdir>` 直接能用
    （`kindletool info` 才要 16 位序列号）。

---

## 没验证的部分

诚实说明，免得下一个接手的人以为都是好的：

1. **真机上一行代码都没跑过**。所有验证都在 QEMU 里完成。真机的越狱状态、
   KUAL 是否装好，都没确认过。

2. **模拟器验证不了电子墨水特性**。模拟器内核是 Debian 6.12，没有 `hwtcon` 驱动，
   `/dev/fb0` 和 EPDC ioctl 全靠 `fbshim` 伪造（它把任何未知 ioctl 都当「刷新屏幕」处理）。
   **能验证**：应用逻辑、排版、缩放、触摸交互、HTTP。
   **不能验证**：电子墨水的刷新时序、波形模式（GC16/A2/DU）、残影、功耗。

3. **`slint-backend-kindle` 没有官方支持 PW5**。它的「已测试设备」是
   PW1 / PW3 / PW4 / Kindle Touch 4 / KOA3。PW5 不在名单里。
   但代码里有 `MXCFB_SEND_UPDATE_MTK` 这条路径，而且内核侧反汇编确认了
   ioctl 编号一致 —— 机制上应该能跑，**但没在真机上验证过**。

4. **模拟器里的触摸与真机手感不同**。真机是电容屏 + `gt9xx`，响应和手势
   跟鼠标点按不一样。

5. **Qt / GTK 这条路没测**。环境里装了 `meson`/`ninja`/`pkg-config`，理论上能编 C/C++ GUI，
   但因为 `KSDK` 没装成，缺库和 pkg-config 文件，没验证过。

---

## 文件说明

```
.\
├── README.md                    ← 本文件
├── PROGRESS.md                  搭建过程记录（含更细的排查细节）
├── Cargo.toml                   三级构建配置（feature 门控）
├── build.rs                     编译 ui/app.slint
├── .cargo/config.toml           模拟器目标用 kindlehf 工具链当链接器
├── ui/app.slint                 占位 UI
├── src/main.rs                  占位客户端
├── fonts/                       必须打包的字体（编进二进制）
├── kual/
│   ├── extensions/kindle-manga/
│   │   ├── config.xml           KUAL 扩展元信息
│   │   ├── menu.json            KUAL 菜单项
│   │   ├── launch.sh            启动脚本（暂停框架 → 跑应用 → 恢复）
│   │   └── show-log.sh          把日志打到屏幕上
│   └── package-kual.sh          打包脚本
└── emulator/
    ├── build-emulator.sh        构建 shim + 应用 + rootfs 副本 + initramfs
    ├── run-emulator.sh          启动 QEMU + 显示窗口
    ├── verify-all.sh            全量自检（推荐先跑）
    ├── guest-init.sh            客体内启动脚本（含各种测试模式）
    ├── fbshim.c                 伪造 /dev/fb0，把帧推给宿主
    ├── touchshim.c              伪造触摸设备，把宿主点击变成 evdev 事件
    ├── lfs-compat.c             64 位符号转发（必须排 LD_PRELOAD 最前）
    ├── viewer.py                宿主窗口（显示 + 鼠标转触摸）
    ├── fbcheck.py               无头帧检查
    ├── mock-server.py           假服务端（实现 /img/<id>/<page>）
    ├── make-test-pages.py       生成 PW5 尺寸的测试 JPEG
    └── *-test.sh / *-test.py    各项测试
```

`emulator/fbshim.c` 来自 [kiansheik/qemu-kindle](https://github.com/kiansheik/qemu-kindle)；
`emulator/touchshim.c` 是**重写的** —— 上游那个文件和 `fbshim.c` 内容完全相同（md5 一致），
是个复制品，里面没有任何触摸逻辑。

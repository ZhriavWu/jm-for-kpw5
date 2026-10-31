# 这份 vendor 副本改了什么

上游：`slint-backend-kindle` **0.7.1**（MIT OR Apache-2.0）
来源：crates.io（本机 Tuna 镜像）

只有**一处功能性改动**：定期用全屏刷新清电子墨水的残影。

---

## 为什么非改不可

上游 `src/platform.rs` 的绘制回调里是这样的：

```rust
if rotation_changed {
    frame_buffer.refresh_full();       // UPDATE_MODE_FULL + GC16
} else {
    frame_buffer.refresh_region(..);   // UPDATE_MODE_PARTIAL + AUTO
}
```

也就是说 **除了启动和屏幕旋转，永远走局部刷新**。

局部刷新快、不闪，但会累积残影 —— 每次只重画变化的那块，旧像素的电荷残留
一点，攒够几十次上一页的字就透出来了。漫画是大面积灰度图，最明显。
清残影只能靠全屏刷新（`UPDATE_MODE_FULL` + GC16 波形），代价是整屏黑一下。

### 为什么应用层绕不过去

- `Framebuffer` 和 `refresh_full()` 都是 `pub(crate)`，公开 API 只有
  `set_rotation` / `set_black_and_white` / `register_font_from_memory`
- 让整屏变脏也没用：脏矩形只决定**刷哪块**，波形仍然是 `AUTO + PARTIAL`
- 拿 `set_rotation` 骗出 `rotation_changed` 也不行：任何能触发它的值
  同时会改变渲染朝向（`degrees % 360`，绕不开）

关键点是**必须「用全刷代替这一帧的局部刷」，而不是刷两遍** ——
刷两遍等于同一帧多闪一次、还慢一倍。要做到「代替」，判断就得发生在
发 ioctl 之前，也就是必须在后端里。

---

## 改了什么

### 新增 `src/refresh.rs`

`RefreshPolicy`：局部刷新计数器 + 间隔配置。

```rust
pub fn set_interval(&self, n: u32)      // 0 = 关掉，回到上游行为
pub fn interval(&self) -> u32
pub fn force_next_refresh(&self)        // 下一次无条件全刷（一发即止）
pub(crate) fn wants_full_refresh(&self) // 事件循环每帧问一次
pub(crate) fn note_full_refresh(&self)  // 已经全刷过了，计数归零
```

`set_interval` **不会**顺手强制全刷一次。分开的理由：启动装载配置时也走
`set_interval`，那时强制全刷纯属浪费（后端进事件循环前已经全刷过一次）。
想立刻看到效果就另外调 `force_next_refresh()`。

### `src/lib.rs`（约 15 行）

- `mod refresh;` + `pub use refresh::{RefreshPolicy, DEFAULT_INTERVAL as
  DEFAULT_FULL_REFRESH_INTERVAL, MAX_INTERVAL};`
- `KindleBackend` 增加 `refresh_policy: Arc<RefreshPolicy>` 字段
- 新增 `pub fn full_refresh_policy(&self) -> Arc<RefreshPolicy>`
  （返回 `Arc`，可以克隆给别的线程）
- `install()` 里创建它并传给 `KindlePlatform::new`
- `into_state()` 跟着带上这个字段

### `src/platform.rs`（约 10 行）

- `KindlePlatform` 增加 `refresh_policy: Arc<RefreshPolicy>` 字段
- `new()` 多一个参数
- 绘制回调里的刷新决定：

```rust
if size.width == 0 || size.height == 0 {
    // 没有脏区域，不刷新，也不计数
} else if rotation_changed {
    frame_buffer.refresh_full();
    self.refresh_policy.note_full_refresh();
} else if self.refresh_policy.wants_full_refresh() {
    log::debug!("全屏刷新（按策略清残影）");
    frame_buffer.refresh_full();
} else {
    frame_buffer.refresh_region(origin, size);
}
```

### `Cargo.toml`（我们的文件，不是上游的）

去掉了上游的 `[workspace] members = ["examples/*"]`（我们不带 examples），
加了 `[lib] doctest = false`（上游模块级文档示例 `include_bytes!` 了他们仓库
里的 `ui/MyFont.ttf`，vendor 副本里没有，doc 测试必然失败；关掉它可以让
`src/` 保持与上游逐字节一致）。

---

## 其余文件与上游逐字节一致

`src/touch.rs`、`src/power.rs`、`src/wakeup.rs`、`src/framebuffer/*` 都没动。
升级时可以这样确认：

```bash
R=<crates.io 上解压出来的 slint-backend-kindle-0.7.1>
for f in src/touch.rs src/power.rs src/wakeup.rs src/framebuffer/mod.rs src/framebuffer/ffi.rs; do
    diff -q "$R/$f" "vendor/slint-backend-kindle/$f" && echo "OK $f"
done
```

---

## 升级上游时怎么重新套

1. 下载新版，整目录覆盖 `vendor/slint-backend-kindle/`
2. 从本文件复制那份 `Cargo.toml` 的头部（去掉 `[workspace]`、加 `doctest = false`）
3. 把 `src/refresh.rs` 拷回去
4. 按上面三处改动重新接线（`lib.rs` 两处、`platform.rs` 三处）；
   新版若改了绘制回调的结构，比对 `refresh_policy.wants_full_refresh()` 的
   插入点即可
5. `cargo test --workspace --target x86_64-unknown-linux-gnu`
   —— `refresh.rs` 里那 8 条测试会验证策略逻辑
6. 真机上确认翻页时屏幕会闪（全刷的特征）

## 应用侧

设置存在程序目录的 `refresh.txt`（一个十进制数字），
界面入口在「设置 → 全屏刷新」。桌面预览没有电子墨水屏，这一项会隐藏
（`src/main.rs` 的 `RefreshHandle::available`）。

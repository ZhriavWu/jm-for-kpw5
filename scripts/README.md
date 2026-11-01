# scripts/ —— 开发与排查脚本

根目录只留三个**每天都在用**的：

| 脚本 | 干什么 |
|---|---|
| `check-ui-spec.sh` | UI 规格检查（20 条，改界面必跑） |
| `check-font-coverage.sh` | 字体覆盖（缺字渲染成空白且不报错，加新文字必跑） |
| `dev-ui.sh` | 编译并启动桌面预览（被 `_gui.sh` 调用） |

其余都在这里。它们**不是产品的一部分**，只服务于开发和排查。

> 所有脚本都会自己算出项目根（`cd "$(dirname "$0")/.."`），
> 在哪个目录下调用都行：`bash scripts/_test.sh`。

---

## 构建 / 测试

| 脚本 | 干什么 |
|---|---|
| `_force_build.sh` | 强制重编桌面版。**推进 `.slint` 的 mtime**，因为 cargo 的增量判断有时会「空转」——输出看着像成功，其实一行都没编 |
| `_build.sh` / `_build_check.sh` | 普通构建 / 构建后核对产物 |
| `_final_build.sh` | 全套验收 + 打包（编译 → 测试 → 规格 → 字体 → KUAL 打包） |
| `_test.sh` | 跑全部 Rust 测试（`--workspace`，含 vendored 后端） |
| `_pkg.sh` | 打包 KUAL 扩展，只看关键几行输出 |
| `_build_server_arm.sh` | 单独交叉编译 ARM 服务端（调试用） |

## 界面 / 交互

| 脚本 | 干什么 |
|---|---|
| `_gui.sh` | 起桌面预览（带上服务端地址，`pkill -x` 清旧实例） |
| `_queue_test.sh` | **慢代理**验证「加载期间点击还灵不灵」。配 `_slow_proxy.py` |
| `_slow_proxy.py` | 把 `/api/search` 拖慢 N 秒，用来复现真机的卡顿 |
| `_caret_test.sh` / `_caret_measure.sh` / `_caret_repro.sh` / `_probe_caret.sh` | 输入框光标：逐字符量物理宽度 |
| `_nav_test.sh` / `_nav_gui.sh` / `_nav_ascii.sh` / `_probe_nav.sh` | 底栏 / 导航 |
| `_star_test.sh` / `_star_measure.py` / `_star_wide.sh` / `_probe_star.sh` | 收藏星的墨量与形状 |
| `_fav_test.sh` / `_fav_measure.sh` / `_check_fav.sh` | 收藏页排版 |

## 崩溃 / 错误

| 脚本 | 干什么 |
|---|---|
| `_panic_repro.py` / `_panic_verify.sh` | 长条漫切屏越界崩溃的复现与验证 |
| `_error_test.sh` / `_error_quit.sh` | 「网络错误」那一屏（注意：要真的停掉服务端才复现得出来，光指死端口没用 —— 广播发现会找到它） |

## 杂项

| 脚本 | 干什么 |
|---|---|
| `_log_scan.sh` | 扫真机日志里的异常 |
| `_cleanup.sh` | 清理残留进程 / 临时文件 |

---

## ⚠️ 几条通用坑（写新脚本时别踩）

1. **`xdotool mousemove` 用屏幕坐标会点不动**（WSLg 下窗口位置每次都变）。
   必须 `xdotool mousemove --window $WID X Y click 1`。
2. **后台起应用要 `nohup`**，否则脚本一退出应用就跟着被杀。
3. **`pkill -x`，不要 `pkill -f`** —— 后者会把命令行里含该字符串的自己的 shell 一起杀掉。
4. **`pgrep -c` 无匹配时既打印 `0` 又返回非零**，`$(pgrep -c X || echo 0)` 会拼出两个 0。
   用 `pgrep -x X | wc -l`。
5. **中文经 PowerShell 传进 WSL 会坏**；带中文的脚本先写成 `.sh` 再执行。

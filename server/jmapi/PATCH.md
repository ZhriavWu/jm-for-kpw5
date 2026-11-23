# `server/jmapi` 的本地补丁（升级上游前必读）

这个目录是 **[laoin114514/jmapi](https://github.com/laoin114514/jmapi) 的本地副本**，
`server/go.mod` 里用 `replace` 指过来：

```go
require github.com/laoin114514/jmapi v0.1.5
replace github.com/laoin114514/jmapi => ./jmapi
```

之所以 vendor 进来，是因为下面这些改动都要动库内部的类型和函数 ——
上游不给这些口子，从外面绕不过去。

> ⚠️ **升级上游 = 这些补丁全部丢失。** 升级前先把这份文档读一遍，
> 按「补丁清单」逐条重新套用，否则：
> * 导出会写出**扩展名 .webp 但内容是 PNG** 的文件（体积 ×60）
> * 导出进度永远是 0/90、**取消按钮失效**
> * 分段数会退回「按 URL 猜」，源站换路径后**图全是花的而且不报错**

---

## 补丁清单

### A. webp 编码支持（`image_decode.go`、`go.mod`）

**问题**：上游只注册了 webp **解码**（`_ "golang.org/x/image/webp"`），
`saveImage` 的 switch 只有 png / gif / jpg，`.webp` 落进 `default` 分支被编成 **PNG**。
结果是「文件名叫 0001.webp，内容其实是 PNG」，体积是 webp 的几十倍。

**改动**：
* 引入 `github.com/deepteams/webp`（纯 Go、零 CGo，能交叉编译 ARM）；
* 新增 `decodeAnyImage()`：先试 deepteams，再退回标准库；
* 新增 `encodeImageWebP()`：按 `ImageOptions` 决定有损 q / 无损；
* `saveImage()` 增加 `case ".webp"`；
* `decodeAndSaveImage` / `saveImageBytes` / `saveImage` 多收一个 `ImageOptions` 参数。

**别忘了**：`jmapi` 是**独立 module**，它自己的 `go.mod` 也要有 deepteams
（父 module 有不算数）。

### B. 可配置的章节详情请求（`option.go`、`downloader.go`）

**问题**：`DownloadPhoto` 里写死 `GetPhotoDetail(id, true, true)`：
* 第 2 个 `true` → 多拉一次 `/album`（实测 3~20 秒）
* 第 3 个 `true` → 响应里没带 scramble_id 时会去调 `/chapter_view_template`
  （HTML、又慢又拿不到值）

**改动**：`DownloadOptions` 增加 `FetchAlbum` / `FetchScrambleID`（默认 false），
`DownloadPhoto` 按它们传参。

### C. 进度 / 取消钩子（`downloader.go`）

**问题**：上游没有任何回调，导出时无法显示进度，也无法中途停止。
Kindle 上一章 90 页要跑一两分钟，用户必须有办法停下来。

**改动**：`Downloader` 增加两个字段（都可为 nil）：
* `OnImage func()` —— 每成功保存一张图调用一次（在下载 goroutine 里）
* `Cancel func() bool` —— 返回 true 时不再派发新任务；**已下完的保留**

调用点：`DownloadPhotoWith` 的任务循环开头、派发任务前、`DownloadAlbum` 派发章节前。

### D. 分段数算法覆盖（`downloader.go`）

**问题**：上游用 `segmentationNumByURL` 从图片 URL 里扒 `/photos/<aid>/` 再算。
一旦源站换了路径结构，就会算出 0 = 不解密，**图是花的而且不报错**。

**改动**：增加 `SegOf func(photo *PhotoDetail, imgName string) int`。
非 nil 且返回值 > 0 时优先用它；否则退回上游算法。

服务端传的是 `segNum(scramble_id, photo_id, 文件名)` —— 和 jmcomic 的
`JmImageTool.get_num` 逐字对齐的那套（见 `server/seg.go`）。

### E. 复用已有的 Client（`downloader.go`）

**问题**：`NewDownloader(option)` 内部会 `option.NewClient()` 再造一个客户端，
等于重新做一遍域名探测（实测 1.7 秒）+ 多一份连接池。

**改动**：新增 `NewDownloaderWithClient(option, client)`；`NewDownloader` 改为
它的薄包装。服务端传自己**已经初始化好的** `s.jc`。

### F. 用已有的章节信息下载（`downloader.go`）

**问题**：`DownloadPhoto(photoID)` 会再请求一次 `/api/photo`，而调用方
（内置服务端）本来就有 `*PhotoDetail` 和 `[]ImageDetail`。

**改动**：拆出 `DownloadPhotoWith(photo, images)`，
`DownloadPhoto` 变成「取详情 + 调它」。

### G. 去掉调试输出（`downloader.go`）

上游 `downloadOneImage` 里有一行 `fmt.Printf("      [debug] ...")`，
每下载一张图就打一行。导出 90 页就是 90 行噪音，删掉。

---

## 历史补丁（本次之前就有，来源见 README）

这三条是拿到这份副本时就带着的，一并记在这里免得升级时漏掉：

| 位置 | 内容 |
|---|---|
| `option.go` 的 `toString` | 把 `"<nil>"` 当空值处理（上游会把 nil 格式化成字符串 `"<nil>"`） |
| `client*.go` | `scramble_id` 的取值 |
| `client*.go` | `/chapter` 的字段取值（`series` / `total_photos`） |

具体行号会随上游变动，升级时按关键词搜。

---

## 升级步骤

1. 备份当前 `server/jmapi/`（`cp -r jmapi jmapi.bak`）
2. 拉上游新版本，覆盖过来
3. 打开这份文档，按 A~G + 历史补丁逐条重新套用
4. `cd server/jmapi && go mod tidy`（**子 module 自己要 tidy**）
5. `cd server && go build ./... && go vet ./... && go test ./...`
6. 跑一遍导出冒烟：`bash /mnt/e/GAME/_e2e_zip.ps1`，
   确认 zip 里是 webp（`method=xxx/xxx` 且大小合理，不是 PNG 那种几十倍体积）
7. 确认 `server/jmapi/go.mod` 里有 deepteams

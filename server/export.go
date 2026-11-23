package main

// 「导出正常图」通道。
//
// # 为什么要它
//
// 缓存里（`cache/<photo_id>/0001.webp`）存的是**源站原图**，也就是被禁漫
// 按横条切乱的那种。用文件浏览器打开是花的。这个模块把图**下载 → 解码 →
// 按 seg 重排 → 重新编码 webp**，然后**打成一个 zip**：
//
//	Kindle/kindle-manga-download/<本子>/<第N话>.zip
//	    └── 0001.webp  0002.webp  …     ← zip 根目录直接就是图片
//
// ⚠️ **必须是 zip，且图片在 zip 根目录**（用户明确要求）：
//    这样用文件浏览器/阅读器打开压缩包，看到的就是一叠能直接看的图，
//    不会多一层嵌套目录。
//
// zip 用 `Store`（不压缩）：webp 本身已经是压缩格式，再 deflate 一遍
// 体积几乎不降，却要多烧 CPU —— 在 Kindle 上这笔账很难看。
//
// # 和阅读链路的关系
//
// **完全独立。** 它不改缓存、不改 /img、不动客户端解密。阅读仍然走
// 「原图 + 客户端解密」，快；导出是用户手动触发的批量活，慢一点没关系。
//
// # 为什么用 deepteams/webp
//
// Go 标准库不能编码 webp（只能解）。要「存成正常 webp、体积几乎不变」
// 就必须有 webp 编码器。deepteams/webp 是纯 Go、零 CGo，能交叉编译到 ARM，
// 实测比 Python/PIL 还快约 1.5~2 倍。

import (
	"archive/zip"
	"bytes"
	"encoding/json"
	"fmt"
	"image"
	"image/draw"
	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"
	"io"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"runtime/debug"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/deepteams/webp"
	"github.com/laoin114514/jmapi"
)

// ---------------------------------------------------------------- 任务状态

type exportFormat string

const (
	exportQ85      exportFormat = "q85"      // 体积优先（实测约原图 112%）
	exportLossless exportFormat = "lossless" // 原画质（实测约原图 2 倍）
)

// exportJob 是给客户端看的进度快照。
type exportJob struct {
	Running  bool   `json:"running"`
	Finished bool   `json:"finished"`
	Done     int    `json:"done"`
	Total    int    `json:"total"`
	Failed   int    `json:"failed"`
	Current  string `json:"current"`
	Dir      string `json:"dir"`
	Error    string `json:"error"`
}

// Exporter 串行执行导出任务。
//
// Kindle 上并发导出没有意义 —— 编解码是 CPU 密集型，多开会互相抢。
// 所以同一时刻只允许一个任务，后来的直接拒绝。
type Exporter struct {
	root string

	mu     sync.Mutex
	job    exportJob
	cancel bool
}

func NewExporter(root string) *Exporter {
	return &Exporter{root: root}
}

func (e *Exporter) snapshot() exportJob {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.job
}

func (e *Exporter) isRunning() bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.job.Running
}

// begin 在**派发 goroutine 之前**就把任务标记成运行中。
//
// 为什么不能等收集完章节再标：收集阶段是一次真实的源站请求（可能 25 秒），
// 这期间如果不标 running，用户再点一次就会又起一个任务。
func (e *Exporter) begin() {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job = exportJob{Running: true}
	e.cancel = false
}

// setRunning 填上总量/当前章节/输出目录，**不动 cancel 和 Running** ——
// 用户可能在收集章节期间就点了取消，这里不能把取消清掉。
func (e *Exporter) setRunning(label string, total int, dir string) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job.Total = total
	e.job.Current = label
	e.job.Dir = dir
}

func (e *Exporter) bump(failed bool) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job.Done++
	if failed {
		e.job.Failed++
	}
}

// bumpFailures 一次性补记 n 个失败页（成功页由 OnImage 回调逐张累加）。
func (e *Exporter) bumpFailures(n int) {
	if n <= 0 {
		return
	}
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job.Done += n
	e.job.Failed += n
}

func (e *Exporter) setCurrent(label string) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job.Current = label
}

func (e *Exporter) finish(errMsg string) {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.job.Running = false
	e.job.Finished = true
	e.job.Error = errMsg
}

func (e *Exporter) cancelled() bool {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.cancel
}

// ---------------------------------------------------------------- 正常图片模式

// handleMode 读取/切换「正常图片模式」。
//
//	GET  /api/mode                → {"ok":true,"normal":false}
//	POST /api/mode {"normal":true}
//
// 这个开关只影响 `/img` 的行为，不影响导出、也不影响缓存里的原图。
func (s *Server) handleMode(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, 200, map[string]any{"ok": true, "normal": s.normal.Load()})
	case http.MethodPost:
		var req struct {
			Normal *bool `json:"normal"`
		}
		if err := json.NewDecoder(io.LimitReader(r.Body, 1<<12)).Decode(&req); err != nil {
			apiError(w, 400, "请求体解析失败："+err.Error())
			return
		}
		if req.Normal == nil {
			apiError(w, 400, "缺少 normal 字段")
			return
		}
		s.normal.Store(*req.Normal)
		log.Printf("正常图片模式：%s", onOff(*req.Normal))
		writeJSON(w, 200, map[string]any{"ok": true, "normal": *req.Normal})
	default:
		apiError(w, 405, "只支持 GET / POST")
	}
}

func onOff(b bool) string {
	if b {
		return "开（/img 解密重排后重新编码 webp）"
	}
	return "关（/img 发原图字节，客户端解密）"
}

// serveNormalImage 正常图片模式下的 /img。
//
// 流程：原图缓存 → 解码 → 按 seg 重排 → 编码 webp q85 → 写 `_normal/` → 发出。
// 产物**单独缓存**，所以每页只在第一次要花那几秒，之后是纯读盘。
func (s *Server) serveNormalImage(w http.ResponseWriter, photoID string, p *jmapi.PhotoDetail, img jmapi.ImageDetail) {
	normPath := s.cache.normalPath(photoID, img.Index)
	if exists(normPath) {
		s.serveImageFile(w, normPath)
		return
	}

	unlock := s.cache.withLock(normPath)
	defer unlock()
	if exists(normPath) {
		s.serveImageFile(w, normPath)
		return
	}

	raw, err := s.ensureRawImage(photoID, img)
	if err != nil {
		http.Error(w, "取图失败："+err.Error(), 502)
		return
	}
	seg := segNum(parseU64(p.ScrambleID), parseU64(p.ID), img.FileName)

	// ⚠️ 同一时刻只处理一张：客户端会预取，多个 /img 同时进来时
	//    每张都要解码一整幅全分辨率图（~9MB RGBA + 重排缓冲 + 编码器工作区）。
	//    真机上并发三张就把系统挤到只剩 12MB，内核 OOM 把 Kindle 框架杀了。
	s.normalMu.Lock()
	data, err := encodeDescrambled(raw, seg, exportQ85)
	s.normalMu.Unlock()
	// 处理完立刻把内存还给系统，别让 Go 的堆一直挂着。
	debug.FreeOSMemory()
	if err != nil {
		log.Printf("正常模式：第 %s/%d 页处理失败（%v）", photoID, img.Index, err)
		http.Error(w, "图片处理失败："+err.Error(), 502)
		return
	}
	if err := s.cache.save(normPath, data); err != nil {
		// 存不下也不影响这一次，只是下次还得重算
		log.Printf("正常模式：写缓存失败（%v）", err)
	} else {
		s.cache.prune()
	}

	w.Header().Set("Content-Type", "image/webp")
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Cache-Control", "public, max-age=86400")
	_, _ = w.Write(data)
}

// serveImageFile 读一个磁盘文件发出去（/img 的两条路径共用收尾逻辑）。
func (s *Server) serveImageFile(w http.ResponseWriter, path string) {
	data, err := os.ReadFile(path)
	if err != nil {
		http.Error(w, "读缓存失败："+err.Error(), 502)
		return
	}
	w.Header().Set("Content-Type", contentTypeOf(path, data))
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Cache-Control", "public, max-age=86400")
	_, _ = w.Write(data)
}

// ---------------------------------------------------------------- HTTP 接口

type exportReq struct {
	PhotoID string `json:"photo_id"`
	AlbumID string `json:"album_id"`
	Scope   string `json:"scope"`  // chapter | album
	Format  string `json:"format"` // q85 | lossless
	// AlbumTitle 客户端已知的本子标题。有就用作目录名。
	//
	// ⚠️ 为什么让客户端带：服务端为了拿标题得现拉 `/api/album`（实测 3~20 秒），
	//    而客户端打开本子时**本来就已经有标题了**。从阅读器发起导出时
	//    服务端内存里也未必有这本子的缓存 —— 之前就退化成 `本子_<id>`。
	AlbumTitle string `json:"album_title"`
}

func (s *Server) handleExport(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, 200, map[string]any{"ok": true, "job": s.export.snapshot()})
	case http.MethodDelete:
		s.export.mu.Lock()
		s.export.cancel = true
		s.export.mu.Unlock()
		writeJSON(w, 200, map[string]any{"ok": true})
	case http.MethodPost:
		var req exportReq
		if err := json.NewDecoder(io.LimitReader(r.Body, 1<<16)).Decode(&req); err != nil {
			apiError(w, 400, "请求体解析失败："+err.Error())
			return
		}
		if req.Scope == "" {
			req.Scope = "chapter"
		}
		if req.Scope == "album" && req.AlbumID == "" {
			apiError(w, 400, "整本导出需要 album_id")
			return
		}
		if req.Scope != "album" && req.PhotoID == "" {
			apiError(w, 400, "导出本话需要 photo_id")
			return
		}
		format := exportFormat(req.Format)
		if format != exportLossless {
			format = exportQ85
		}
		if s.export.isRunning() {
			apiError(w, 409, "已有导出任务在进行，等它完成或取消后再试")
			return
		}
		s.export.begin()
		go s.runExport(req, format)
		writeJSON(w, 200, map[string]any{"ok": true, "job": s.export.snapshot()})
	default:
		apiError(w, 405, "只支持 GET / POST / DELETE")
	}
}

// ---------------------------------------------------------------- 执行导出

// exportChapter 一个待导出的章节。
type exportChapter struct {
	title  string
	detail *jmapi.PhotoDetail
	imgs   []jmapi.ImageDetail
}

func (s *Server) runExport(req exportReq, format exportFormat) {
	dirLabel := "体积优先"
	if format == exportLossless {
		dirLabel = "无损"
	}
	log.Printf("开始导出：scope=%s photo=%s album=%s 格式=%s", req.Scope, req.PhotoID, req.AlbumID, dirLabel)

	var chapters []exportChapter
	var albumTitle string
	var err error

	switch req.Scope {
	case "album":
		chapters, albumTitle, err = s.collectAlbumChapters(req.AlbumID)
	default:
		var ch exportChapter
		ch, albumTitle, err = s.collectChapter(req.PhotoID)
		if err == nil {
			chapters = []exportChapter{ch}
		}
	}
	if err != nil {
		log.Printf("导出失败（收集章节）：%v", err)
		s.export.finish("取章节信息失败：" + err.Error())
		return
	}
	// 客户端给了标题就用它 —— 比服务端猜的 `本子_<id>` 好得多。
	if t := strings.TrimSpace(req.AlbumTitle); t != "" {
		albumTitle = t
	}
	if len(chapters) == 0 {
		s.export.finish("没有可导出的章节")
		return
	}
	// 收集章节期间用户可能已经点了取消
	if s.export.cancelled() {
		s.export.finish("已取消")
		log.Printf("导出已取消（收集章节阶段）")
		return
	}

	total := 0
	for _, c := range chapters {
		total += len(c.imgs)
	}

	albumDir := sanitizeName(albumTitle)
	outRoot := filepath.Join(s.export.root, albumDir)
	s.export.setRunning(chapters[0].title, total, outRoot)

	// 同一本里不同话的标题可能重复（甚至清洗后撞名），
	// 用一个集合保证每个 zip 的文件名唯一。
	used := make(map[string]bool)

	done := 0
	for _, ch := range chapters {
		if s.export.cancelled() {
			s.export.finish("已取消")
			log.Printf("导出已取消（%d/%d）", done, total)
			return
		}
		s.export.setCurrent(ch.title)
		n := s.exportChapter(ch, outRoot, format, used)
		done += n
	}

	if s.export.cancelled() {
		s.export.finish("已取消")
		log.Printf("导出已取消（%d/%d）", done, total)
		return
	}
	s.export.finish("")
	log.Printf("导出完成：%d 页 → %s", total, outRoot)
}

// collectChapter 单话：拿章节详情和图片列表。
func (s *Server) collectChapter(photoID string) (exportChapter, string, error) {
	p, imgs, err := s.photoDetail(photoID)
	if err != nil {
		return exportChapter{}, "", err
	}
	_, title := s.chapterMeta(p)
	if strings.TrimSpace(title) == "" {
		title = "第1话"
	}
	return exportChapter{
		title:  title,
		detail: p,
		imgs:   imgs,
	}, s.albumTitleOf(p), nil
}

// collectAlbumChapters 整本：依次拿每一话的图片列表（已缓存的本子不会联网）。
func (s *Server) collectAlbumChapters(albumID string) ([]exportChapter, string, error) {
	a, err := s.albumDetail(albumID)
	if err != nil {
		return nil, "", err
	}
	albumTitle := a.Name
	out := make([]exportChapter, 0)
	for i, c := range albumChapters(a) {
		if s.export.cancelled() {
			break
		}
		p, imgs, err := s.photoDetail(c.ID)
		if err != nil {
			log.Printf("导出：跳过章节 %s（%v）", c.ID, err)
			continue
		}
		title := strings.TrimSpace(c.Title)
		if title == "" {
			title = fmt.Sprintf("第%d话", i+1)
		}
		out = append(out, exportChapter{
			title:  title,
			detail: p,
			imgs:   imgs,
		})
	}
	return out, albumTitle, nil
}

// albumTitleOf 尽量在**不联网**的前提下拿到本子标题。
func (s *Server) albumTitleOf(p *jmapi.PhotoDetail) string {
	id := p.SeriesID
	if id == "" || id == "0" {
		id = p.ID
	}
	s.albumMu.Lock()
	if e, ok := s.albums[id]; ok && e.album != nil && strings.TrimSpace(e.album.Name) != "" {
		name := e.album.Name
		s.albumMu.Unlock()
		return name
	}
	s.albumMu.Unlock()
	// 拿不到标题就用 ID，至少目录能建出来
	return "本子_" + id
}

// exportChapter 用 jmapi 自带的 Downloader 下载一话，然后打成一个 zip。
//
// **下载这一段不再自己写循环**：库里的 `DownloadPhotoWith` 已经是
// 「N 个并发 worker + 每张图下载/解码/重排/编码 + 失败收集」的完整实现，
// 再写一份只会多一处要维护的重复逻辑。
//
// zip 里**只有图片**，路径就是 `0001.webp` 这种（根目录直接是图）。
func (s *Server) exportChapter(ch exportChapter, outRoot string, format exportFormat, used map[string]bool) int {
	n := len(ch.imgs)
	if n == 0 {
		return 0
	}
	// ⚠️ 必须先把目录建出来，否则下面 MkdirTemp / Create 全会失败，
	//    而且日志里只有一句「建不了」，排查时很容易看漏。
	if err := os.MkdirAll(outRoot, 0o755); err != nil {
		log.Printf("导出：建不了目录 %s（%v）", outRoot, err)
		return 0
	}

	// 先下到临时目录，再打 zip，最后删掉临时目录。
	// 库的 Downloader 只会往文件里写，所以顺着它来。
	work, err := os.MkdirTemp(outRoot, ".work-")
	if err != nil {
		log.Printf("导出：建不了临时目录（%v）", err)
		return 0
	}
	defer os.RemoveAll(work)

	d := s.newExportDownloader(work, format)
	beforeFail := len(d.FailedImages)

	if err := d.DownloadPhotoWith(ch.detail, ch.imgs); err != nil {
		log.Printf("导出：「%s」下载失败（%v）", ch.title, err)
	}
	// 成功页由 OnImage 逐张累加；失败在这里一次性补记
	s.export.bumpFailures(len(d.FailedImages) - beforeFail)

	// 把下好的文件按页序收起来
	files := make([]string, 0, n)
	for _, im := range ch.imgs {
		p := filepath.Join(work, fmt.Sprintf("%05d.webp", im.Index))
		if exists(p) {
			files = append(files, p)
		}
	}
	if len(files) == 0 {
		log.Printf("导出：「%s」一页都没下下来", ch.title)
		return 0
	}
	if s.export.cancelled() {
		// 已下完的留着不打包：半章 zip 比没有更让人困惑
		log.Printf("导出：「%s」已取消（已下 %d 页，不生成 zip）", ch.title, len(files))
		return len(files)
	}

	zipBase := uniqueName(sanitizeName(ch.title), used)
	zipPath := filepath.Join(outRoot, zipBase+".zip")
	tmpPath := zipPath + ".part"

	// 先写 .part 再改名：FAT32 上直接覆写，断电会留下半截 zip。
	if err := writeZip(tmpPath, files); err != nil {
		log.Printf("导出：「%s」打包失败（%v）", ch.title, err)
		_ = os.Remove(tmpPath)
		return 0
	}
	if err := os.Rename(tmpPath, zipPath); err != nil {
		log.Printf("导出：改名失败（%v）", err)
		_ = os.Remove(tmpPath)
		return 0
	}
	log.Printf("导出：%s → %d 页", filepath.Base(zipPath), len(files))
	return len(files)
}

// newExportDownloader 造一个只服务导出的 Downloader。
//
// 复用服务端**已经初始化好的** `s.jc`：`NewDownloader` 会另起一个 Client，
// 重新做一遍域名探测（实测 1.7 秒）+ 多一份连接池，没有理由。
func (s *Server) newExportDownloader(workDir string, format exportFormat) *jmapi.Downloader {
	opt := jmapi.DefaultOption()
	opt.ClientConfig = jmapi.Config{
		ClientType: jmapi.ClientTypeAPI,
		RetryTimes: 3,
		Timeout:    25 * time.Second,
	}
	opt.DirRule = jmapi.DirRule{Rule: "Bd", BaseDir: workDir}
	opt.Download.Image.Decode = true
	opt.Download.Image.Suffix = ".webp"
	opt.Download.Threading.Image = 3
	if format == exportLossless {
		opt.Download.Image.WebpLossless = true
	} else {
		opt.Download.Image.WebpQuality = 85
	}

	d := jmapi.NewDownloaderWithClient(opt, s.jc)
	d.OnImage = func() { s.export.bump(false) }
	d.Cancel = s.export.cancelled
	// 每张图处理完就把内存还给系统 —— 否则 Go 的堆会一直挂着，
	// 在 Kindle 上很快就把内核逼到 OOM（真机上已经发生过一次）。
	d.ReleaseMemory = func() { debug.FreeOSMemory() }
	// 用我们自己那套分段数算法（章节 ID + 原始文件名），不靠 URL 形状猜。
	d.SegOf = func(p *jmapi.PhotoDetail, imgName string) int {
		name := strings.TrimSuffix(imgName, filepath.Ext(imgName))
		return segNum(parseU64(p.ScrambleID), parseU64(p.ID), name)
	}
	return d
}

// writeZip 把若干文件按顺序塞进 zip，条目名就是 `0001.webp`（不带目录）。
func writeZip(path string, files []string) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	zw := zip.NewWriter(f)
	fail := func(e error) error {
		_ = zw.Close()
		_ = f.Close()
		return e
	}
	for i, src := range files {
		data, err := os.ReadFile(src)
		if err != nil {
			return fail(err)
		}
		if err := addZipEntry(zw, zipEntryName(i+1), data); err != nil {
			return fail(err)
		}
	}
	if err := zw.Close(); err != nil {
		_ = f.Close()
		return err
	}
	return f.Close()
}

// zipEntryName 第几页（1 基）在 zip 里的文件名。
//
// **不带任何目录前缀** —— 用户要求解压后根目录直接就是图片。
func zipEntryName(page1Based int) string {
	return fmt.Sprintf("%04d.webp", page1Based)
}

// addZipEntry 往 zip 里塞一张图。
//
// 用 `Store`（不压缩）：webp 已经是压缩格式，deflate 基本没用，
// 反而在 Kindle 上白烧 CPU。用户也说过「压缩不压缩都可以」。
func addZipEntry(zw *zip.Writer, name string, data []byte) error {
	h := &zip.FileHeader{
		Name:   name, // 根目录直接是图片，不带任何前缀
		Method: zip.Store,
	}
	h.SetMode(0o644)
	w, err := zw.CreateHeader(h)
	if err != nil {
		return err
	}
	_, err = w.Write(data)
	return err
}

// uniqueName 在 used 里占一个不重复的名字。
func uniqueName(base string, used map[string]bool) string {
	name := base
	for i := 2; used[name]; i++ {
		name = fmt.Sprintf("%s (%d)", base, i)
	}
	used[name] = true
	return name
}

// ---------------------------------------------------------------- 图像处理

// encodeDescrambled 解码 → 按 seg 重排 → 编码 webp。
func encodeDescrambled(raw []byte, seg int, format exportFormat) ([]byte, error) {
	img, err := decodeImage(raw)
	if err != nil {
		return nil, err
	}
	if seg > 1 {
		img = rearrangeRows(img, seg)
	}

	var buf bytes.Buffer
	opts := &webp.EncoderOptions{Quality: 85, Method: 4}
	if format == exportLossless {
		opts = &webp.EncoderOptions{Lossless: true, Quality: 75}
	}
	if err := webp.Encode(&buf, img, opts); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// decodeImage 先试 webp（deepteams 支持有损/无损/动画），再退回标准库。
func decodeImage(raw []byte) (image.Image, error) {
	if img, err := webp.Decode(bytes.NewReader(raw)); err == nil {
		return img, nil
	}
	img, _, err := image.Decode(bytes.NewReader(raw))
	return img, err
}

// rearrangeRows 把横条切乱的图拼回去。
//
// 算法与 `jmcomic` 的 `JmImageTool.decode_and_save`、以及
// `jmapi.rearrangeImageBySegmentation` **完全一致**：
//
//	第 i 块的目标位置从 piece*i（+over）开始，源位置从 h-piece*(i+1)-over 开始。
func rearrangeRows(src image.Image, num int) image.Image {
	b := src.Bounds()
	w, h := b.Dx(), b.Dy()
	if num <= 1 || h < num {
		return src
	}
	over := h % num
	piece := h / num
	dst := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := 0; i < num; i++ {
		move := piece
		ySrc := h - piece*(i+1) - over
		yDst := piece * i
		if i == 0 {
			move += over
		} else {
			yDst += over
		}
		srcRect := image.Rect(0, ySrc, w, ySrc+move)
		dstRect := image.Rect(0, yDst, w, yDst+move)
		draw.Draw(dst, dstRect, src, srcRect.Min, draw.Src)
	}
	return dst
}

// ---------------------------------------------------------------- 小工具

// ensureRawImage 取一页原图字节：缓存命中直接读，否则下载并写缓存。
//
// ⚠️ 和 handleImage 里那段逻辑等价。刻意各写一份而不是抽公共函数，
//    是为了不动已经跑通的 /img 关键路径；两边都靠 `cache.withLock` 保证
//    同一页只下载一次。
func (s *Server) ensureRawImage(photoID string, img jmapi.ImageDetail) ([]byte, error) {
	path := s.cache.path(photoID, img.Index, img.Suffix)
	if exists(path) {
		if b, err := os.ReadFile(path); err == nil {
			return b, nil
		}
	}
	unlock := s.cache.withLock(path)
	defer unlock()
	if exists(path) {
		if b, err := os.ReadFile(path); err == nil {
			return b, nil
		}
	}
	jc, err := s.upstream()
	if err != nil {
		return nil, err
	}
	data, err := jc.DownloadImage(img.DownloadURL)
	if err != nil {
		return nil, fmt.Errorf("下载失败：%w", err)
	}
	if len(data) == 0 {
		return nil, errEmptyDownload
	}
	if err := s.cache.save(path, data); err != nil {
		return nil, fmt.Errorf("写缓存失败：%w", err)
	}
	s.cache.prune()
	return data, nil
}

// sanitizeName 把标题变成合法的目录名。
//
// FAT32 不能有 `\ / : * ? " < > |`，也不能以点或空格结尾。这里换成全角，
// 既合法又保留可读性。太长截断（FAT32 单个名字 255 字节）。
func sanitizeName(s string) string {
	s = strings.TrimSpace(s)
	if s == "" {
		return "未命名"
	}
	repl := strings.NewReplacer(
		"\\", "＼", "/", "／", ":", "：", "*", "＊", "?", "？",
		"\"", "＂", "<", "＜", ">", "＞", "|", "｜",
		"\n", " ", "\r", " ", "\t", " ",
	)
	s = repl.Replace(s)
	s = strings.TrimRight(s, ". ")
	if s == "" {
		return "未命名"
	}
	// 按字符截断，别切坏 UTF-8
	if utf8.RuneCountInString(s) > 60 {
		r := []rune(s)
		s = string(r[:60])
	}
	return s
}

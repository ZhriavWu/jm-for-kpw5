// kindle-manga-server —— 内置到 Kindle 的 Go 服务端。
//
// # 它做什么 / 不做什么
//
// 只做三件事：**下载、存盘、原样发出去**。零解码、零缩放、零重编码。
// 禁漫的「横条重排」解密由客户端做（见客户端 src/descramble.rs），
// 所以这里发出去的 webp 就是源站原样字节。
//
// # 为什么要内置
//
// 原来服务端跑在电脑上，Kindle 一离开电脑就没法看。内置之后整套东西
// 只依赖 Kindle 自己（外加 WiFi）。代价是 Kindle 上多一个 ~9 MB 的进程
// 和一份图片缓存。
//
// # 和客户端的分工
//
//	Go 服务端 (127.0.0.1:8001)          Rust 客户端
//	  下载原图 → 存盘 → 原样发  ──HTTP──►  解码 → 解密 → 缩放 → 显示
//
// # 缓存
//
//	cache/<photo_id>/0001.webp     源站原图，原样
//	cache/_covers/<album_id>.jpg   封面
//
// 单页 200~350 KB，默认上限 2 GB（`-cache-max-mb`），超了按最久未用删。
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/laoin114514/jmapi"
)

const (
	version = "2.0.0"

	// 搜索每页显示几条。和客户端搜索页的三列网格（每屏 6 格）绑定。
	// 改这里要同时看 kindle-manga 的 SEARCH_PER_PAGE（Python 版）和
	// 客户端 ui/app.slint 的网格：6 条 = 2 行 × 3 列，正好铺满。
	perPage = 6
	// 禁漫 API 一次固定返回 80 条，我们再切成显示页。
	apiPageSize = 80

	screenW = 1236
	screenH = 1648

	defaultSort = "mr"
)

// 排序方式和显示名。顺序就是客户端按钮的顺序，**客户端不写死**。
var sortOptions = []map[string]string{
	{"value": "mr", "label": "最新"},
	{"value": "tf", "label": "最多喜欢"},
	{"value": "tr", "label": "最高评分"},
	{"value": "mv", "label": "最多观看"},
}

type Server struct {
	// 上游客户端**异步**构造。
	//
	// ⚠️ 为什么不能同步构造：`jmapi.NewClient(AutoUpdateHost: true)` 会去
	//    更新域名列表，正常 0.45 秒，网络差时可能要几十秒。而它排在
	//    `net.Listen` **前面**的话，那段时间端口根本没在听 ——
	//    客户端连上来直接是 Connection refused，表现就是「阅读界面打不开」
	//    （其实是整个服务端没起来）。
	//
	//    而「不更新域名」也不行：实测默认域名返回 HTML（被墙/失效），
	//    请求全是 `invalid character '<'`，14 秒后失败。
	//
	//    所以：**先监听，再在后台构造**。`/api/info` 不依赖上游，
	//    客户端探活立刻成功；真正要取数据的接口等 `jcReady`。
	jc      *jmapi.Client
	jcReady chan struct{}
	jcErr   error
	cache   *Cache
	hist    *History

	// jmapi 的元数据调用会改 Client 内部的域名等状态，串行化最稳。
	// 下载图片不经过这里（http.Client 本身是并发安全的）。
	jmu sync.Mutex

	photos  map[string]*photoEntry
	photoMu sync.Mutex

	albums  map[string]*albumEntry
	albumMu sync.Mutex

	search *SearchCache

	start time.Time
}

type photoEntry struct {
	at     time.Time
	detail *jmapi.PhotoDetail
	images []jmapi.ImageDetail
}

type albumEntry struct {
	at    time.Time
	album *jmapi.AlbumDetail
}

func main() {
	addr := flag.String("addr", "127.0.0.1:8001", "监听地址（默认只听本机）")
	cacheDir := flag.String("cache", envOr("JM_CACHE_DIR", defaultCacheDir()), "图片缓存目录")
	histPath := flag.String("history", envOr("JM_HISTORY", "history.json"), "观看记录文件")
	readyFile := flag.String("ready", "", "启动成功后写这个文件（给启动脚本判就绪用）")
	cacheMaxMB := flag.Int("cache-max-mb", 1024, "缓存上限（MB），0 = 不限制")
	flag.Parse()

	log.SetFlags(log.LstdFlags)
	log.Printf("kindle-manga-server %s 启动：addr=%s cache=%s history=%s", version, *addr, *cacheDir, *histPath)

	cache, err := NewCache(*cacheDir, int64(*cacheMaxMB)*1024*1024)
	if err != nil {
		log.Fatalf("缓存目录不可用：%v", err)
	}
	if n := cache.prune(); n > 0 {
		log.Printf("启动清理：删了 %d 个旧缓存文件", n)
	}

	s := &Server{
		jcReady: make(chan struct{}),
		cache:   cache,
		hist:    NewHistory(*histPath),
		photos:  make(map[string]*photoEntry),
		albums:  make(map[string]*albumEntry),
		search:  NewSearchCache(),
		start:   time.Now(),
	}

	// 后台把上游客户端准备好。跑完 close(jcReady)，之后所有 handler 正常用。
	go func() {
		t0 := time.Now()
		s.jc = jmapi.NewClient(jmapi.Config{
			ClientType:     jmapi.ClientTypeAPI,
			RetryTimes:     3,
			Timeout:        25 * time.Second,
			AutoUpdateHost: true,
		})
		log.Printf("上游客户端就绪（%.2fs，%d 个域名）", time.Since(t0).Seconds(), len(s.jc.Domains()))
		close(s.jcReady)
	}()

	ln, err := net.Listen("tcp", *addr)
	if err != nil {
		log.Fatalf("监听 %s 失败：%v", *addr, err)
	}
	if *readyFile != "" {
		if err := os.WriteFile(*readyFile, []byte(strconv.Itoa(os.Getpid())), 0o644); err != nil {
			log.Printf("写就绪文件失败：%v", err)
		}
	}

	srv := &http.Server{
		Handler:           s.routes(),
		ReadHeaderTimeout: 10 * time.Second,
	}

	go func() {
		log.Printf("就绪：http://%s", ln.Addr())
		if err := srv.Serve(ln); err != nil && err != http.ErrServerClosed {
			log.Fatalf("服务退出：%v", err)
		}
	}()

	// 启动脚本会 kill 我们，走优雅退出，别把正在写的缓存文件留成半截。
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	<-stop
	log.Printf("收到退出信号，正在关闭…")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = srv.Shutdown(ctx)
}

func envOr(key, def string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return def
}

// 默认缓存目录：真机上放 /mnt/us（用户分区，空间大），
// 桌面调试时放当前目录，免得在 Windows 上建 /mnt/us。
func defaultCacheDir() string {
	if runtime.GOOS == "linux" {
		if st, err := os.Stat("/mnt/us"); err == nil && st.IsDir() {
			return "/mnt/us/kindle-manga-cache"
		}
	}
	return "cache"
}

// ---------------------------------------------------------------- 路由

func (s *Server) routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/info", s.handleInfo)
	mux.HandleFunc("/api/search", s.handleSearch)
	mux.HandleFunc("/api/album/", s.handleAlbum)
	mux.HandleFunc("/api/photo/", s.handlePhoto)
	mux.HandleFunc("/api/chapter/", s.handlePhoto) // 别名，一次拿齐
	mux.HandleFunc("/api/history", s.handleHistory)
	mux.HandleFunc("/api/history/", s.handleHistory)
	mux.HandleFunc("/api/prefetch", s.handlePrefetch)
	mux.HandleFunc("/api/status", s.handleStatus)
	mux.HandleFunc("/api/cache", s.handleCache)
	mux.HandleFunc("/img/", s.handleImage)
	mux.HandleFunc("/cover/", s.handleCover)
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" {
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		fmt.Fprintf(w, "kindle-manga-server %s\n运行了 %s\n", version,
			time.Since(s.start).Round(time.Second))
	})
	return logRequests(mux)
}

func logRequests(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		t0 := time.Now()
		next.ServeHTTP(w, r)
		// 图片请求很频繁，只在慢的时候记一行，免得日志刷爆（Kindle 闪存慢）。
		if d := time.Since(t0); d > 3*time.Second {
			log.Printf("慢请求 %s %s（%s）", r.Method, r.URL.Path, d.Round(time.Millisecond))
		}
	})
}

// ---------------------------------------------------------------- /api/info

func (s *Server) handleInfo(w http.ResponseWriter, r *http.Request) {
	writeJSON(w, 200, map[string]any{
		"ok":              true,
		"service":         "jm-kindle",
		"version":         version,
		"base_url":        "http://" + r.Host,
		"screen":          map[string]int{"width": screenW, "height": screenH},
		"search_per_page": perPage,
		"sort_options":    sortOptions,
		"default_sort":    defaultSort,
		"endpoints": []string{
			"GET  /api/info",
			"GET  /api/search?q=&o=&p=",
			"GET  /api/album/<album_id>",
			"GET  /api/photo/<photo_id>",
			"GET  /api/chapter/<photo_id>",
			"GET  /api/history",
			"POST /api/history   {id, title}",
			"DEL  /api/history/<album_id>",
			"GET  /img/<photo_id>/<page>",
			"GET  /cover/<album_id>",
			"GET  /api/cache               用量统计",
			"POST /api/cache  {max_mb}     改上限（会立刻清理）",
			"POST /api/cache  {clear:true} 清空磁盘缓存",
		},
		"notes": []string{
			"服务端不解码：/img 发的是源站原图（多半是 webp），解密在客户端",
			"/api/photo 的每页带 seg（分割数），客户端据此做横条重排",
			"页码从 0 开始；越界返回 404",
			"max_mb = 0 表示不限制；超上限时按最久未访问删除",
		},
	})
}

// upstream 等上游客户端就绪后返回它。
//
// 首次调用可能要等几秒（后台正在更新域名列表）。等不到就返回错误，
// 让请求以 502 明确失败，而不是无限期挂着 —— 客户端自己有超时，
// 两边都挂住的话用户只能看到转圈的加载提示。
func (s *Server) upstream() (*jmapi.Client, error) {
	select {
	case <-s.jcReady:
		return s.jc, nil
	case <-time.After(90 * time.Second):
		return nil, fmt.Errorf("上游客户端还没就绪（网络太慢？）")
	}
}

// ---------------------------------------------------------------- 工具

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(code)
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	_ = enc.Encode(v)
}

func apiError(w http.ResponseWriter, code int, msg string) {
	writeJSON(w, code, map[string]any{"ok": false, "error": msg})
}

// 从路径里取出最后一段（`/api/album/123/` → `123`）。
func lastSegment(p string) string {
	p = strings.Trim(p, "/")
	if i := strings.LastIndexByte(p, '/'); i >= 0 {
		return p[i+1:]
	}
	return p
}

func asSlice(v any) []any {
	s, _ := v.([]any)
	return s
}

func asStr(v any) string {
	switch t := v.(type) {
	case nil:
		return ""
	case string:
		return strings.TrimSpace(t)
	case float64:
		return strconv.FormatFloat(t, 'f', -1, 64)
	case int:
		return strconv.Itoa(t)
	case json.Number:
		return t.String()
	default:
		s := fmt.Sprintf("%v", t)
		if s == "<nil>" {
			return ""
		}
		return strings.TrimSpace(s)
	}
}

func asInt(v any) int {
	n, err := strconv.Atoi(asStr(v))
	if err != nil {
		return 0
	}
	return n
}

// asStringSlice 把 `["a","b"]` 收成 `[]string`，顺手去掉空项和首尾空白。
//
// 上游的 actors / works 是字符串数组，但偶尔会混进 null 或空串
// （实测 `works` 里出现过空字符串），空标签画出来就是个空框。
func asStringSlice(v any) []string {
	out := make([]string, 0)
	for _, item := range asSlice(v) {
		if s := asStr(item); s != "" {
			out = append(out, s)
		}
	}
	return out
}

// ---------------------------------------------------------------- 预取 / 状态（兼容接口）

func (s *Server) handlePrefetch(w http.ResponseWriter, r *http.Request) {
	// 客户端目前不用这个接口（它只按页取）。返回成功，不做实际动作 ——
	// 服务端缓存本来就是「谁取谁缓存」，预取对本地服务端没有意义。
	writeJSON(w, 200, map[string]any{"ok": true, "scheduled": 0, "note": "本地服务端按需下载，无需预取"})
}

func (s *Server) handleStatus(w http.ResponseWriter, r *http.Request) {
	photo := strings.TrimSpace(r.URL.Query().Get("photo"))
	if photo == "" {
		apiError(w, 400, "缺少参数 photo")
		return
	}
	dir := filepath.Join(s.cache.dir, photo)
	pages := []int{}
	if entries, err := os.ReadDir(dir); err == nil {
		for _, e := range entries {
			if e.IsDir() {
				continue
			}
			name := strings.TrimSuffix(e.Name(), filepath.Ext(e.Name()))
			if n, err := strconv.Atoi(name); err == nil {
				pages = append(pages, n)
			}
		}
	}
	writeJSON(w, 200, map[string]any{"ok": true, "photo": photo, "cached_pages": pages})
}

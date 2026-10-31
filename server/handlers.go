package main

import (
	"encoding/json"
	"io"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/laoin114514/jmapi"
)

// ---------------------------------------------------------------- 搜索

func (s *Server) handleSearch(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	keyword := strings.TrimSpace(q.Get("q"))
	if keyword == "" {
		apiError(w, 400, "缺少参数 q")
		return
	}
	order := q.Get("o")
	valid := false
	for _, o := range sortOptions {
		if o["value"] == order {
			valid = true
			break
		}
	}
	if !valid {
		order = defaultSort
	}
	page, err := strconv.Atoi(q.Get("p"))
	if err != nil || page < 1 {
		page = 1
	}

	items, page, pages, total, err := s.searchSlice(keyword, order, page)
	if err != nil {
		apiError(w, 502, "搜索失败："+err.Error())
		return
	}

	out := make([]map[string]any, 0, len(items))
	for _, it := range items {
		out = append(out, map[string]any{
			"id":    it.ID,
			"title": it.Name,
			"cover": "/cover/" + it.ID,
		})
	}
	var pageCount any // 上游没给总数时是 null，客户端据此不显示「下一页」
	if pages > 0 {
		pageCount = pages
	}
	var totalVal any
	if total > 0 {
		totalVal = total
	}
	writeJSON(w, 200, map[string]any{
		"ok":         true,
		"keyword":    keyword,
		"order":      order,
		"page":       page,
		"page_count": pageCount,
		"total":      totalVal,
		"items":      out,
	})
}

// ---------------------------------------------------------------- 本子

type chapterInfo struct {
	ID    string
	Index int
	Title string
}

// albumChapters 从 `/album` 的 `series` 字段里取章节。
//
// ⚠️ 上游字段名是 `series`，**不是** `episode_list` —— jmapi 库只认后者，
//    所以这里直接读 `Raw`。单话本子的 `series` 是空数组，这时把本子自己
//    当成唯一一话（jm 的约定：单话本子的 album id 就是 photo id）。
func albumChapters(a *jmapi.AlbumDetail) []chapterInfo {
	if raw := asSlice(a.Raw["series"]); len(raw) > 0 {
		out := make([]chapterInfo, 0, len(raw))
		for _, item := range raw {
			m, ok := item.(map[string]any)
			if !ok {
				continue
			}
			id := asStr(m["id"])
			if id == "" {
				continue
			}
			idx := asInt(m["sort"])
			if idx <= 0 {
				idx = len(out) + 1
			}
			out = append(out, chapterInfo{ID: id, Index: idx, Title: asStr(m["name"])})
		}
		if len(out) > 0 {
			return out
		}
	}
	return []chapterInfo{{ID: a.ID, Index: 1, Title: a.Name}}
}

func (s *Server) albumDetail(id string) (*jmapi.AlbumDetail, error) {
	s.albumMu.Lock()
	if e, ok := s.albums[id]; ok && time.Since(e.at) < 30*time.Minute {
		s.albumMu.Unlock()
		return e.album, nil
	}
	s.albumMu.Unlock()

	jc, err := s.upstream()
	if err != nil {
		return nil, err
	}
	s.jmu.Lock()
	a, err := jc.GetAlbumDetail(id)
	s.jmu.Unlock()
	if err != nil {
		return nil, err
	}

	s.albumMu.Lock()
	if len(s.albums) > 100 {
		s.albums = make(map[string]*albumEntry)
	}
	s.albums[id] = &albumEntry{at: time.Now(), album: a}
	s.albumMu.Unlock()
	return a, nil
}

func (s *Server) handleAlbum(w http.ResponseWriter, r *http.Request) {
	id := lastSegment(strings.TrimPrefix(r.URL.Path, "/api/album/"))
	if id == "" {
		apiError(w, 400, "缺少本子 ID")
		return
	}
	a, err := s.albumDetail(id)
	if err != nil {
		apiError(w, 502, "取本子失败："+err.Error())
		return
	}

	chs := albumChapters(a)
	chapters := make([]map[string]any, 0, len(chs))
	for _, c := range chs {
		chapters = append(chapters, map[string]any{
			"id":         c.ID,
			"index":      strconv.Itoa(c.Index),
			"title":      c.Title,
			"indextitle": "",
		})
	}

	writeJSON(w, 200, map[string]any{
		"ok":            true,
		"id":            a.ID,
		"title":         a.Name,
		"author":        strings.Join(a.Author, " / "),
		"cover":         "/cover/" + a.ID,
		"page_count":    asInt(a.Raw["total_photos"]),
		"chapter_count": len(chapters),
		"tags":          a.Tags,
		"chapters":      chapters,
	})
}

// ---------------------------------------------------------------- 章节

func (s *Server) photoDetail(id string) (*jmapi.PhotoDetail, []jmapi.ImageDetail, error) {
	s.photoMu.Lock()
	if e, ok := s.photos[id]; ok && time.Since(e.at) < 30*time.Minute {
		s.photoMu.Unlock()
		return e.detail, e.images, nil
	}
	s.photoMu.Unlock()

	// ⚠️ `fetchScrambleID` 传 false：`/chapter` 的响应里本来就带 scramble_id
	//    （实测所有本子都有）。传 true 的话，一旦某次响应没带，库会去调
	//    `/chapter_view_template` —— 那个接口又慢又拿不到东西，是纯浪费。
	//    真拿不到时用 jmcomic 的兜底值 220980（见下方 segNum 调用）。
	jc, err := s.upstream()
	if err != nil {
		return nil, nil, err
	}
	s.jmu.Lock()
	p, err := jc.GetPhotoDetail(id, false, false)
	s.jmu.Unlock()
	if err != nil {
		return nil, nil, err
	}
	if p.ScrambleID == "" {
		p.ScrambleID = fallbackScrambleID
	}
	imgs, err := p.Images()
	if err != nil {
		return nil, nil, err
	}

	s.photoMu.Lock()
	if len(s.photos) > 200 {
		s.photos = make(map[string]*photoEntry)
	}
	s.photos[id] = &photoEntry{at: time.Now(), detail: p, images: imgs}
	s.photoMu.Unlock()
	return p, imgs, nil
}

// chapterMeta 给多话本子的一话补上「第几话」和章节名。
//
// ⚠️ **只查已经缓存的本子，绝不现拉网络请求。**
//
//    踩过：这里原来会 `albumDetail(seriesID)`，而它缓存未命中时是一次
//    真实的源站请求（实测 /album 3~20 秒）。客户端点一话要等「章节详情 +
//    本子详情」两次往返，表现就是「阅读界面打不开」。
//    而客户端自己会从 `/api/album` 的 chapters 里按 id 找章节名
//    （见 main.rs 的 `chapter_name()`），所以这里查不到完全不影响显示。
func (s *Server) chapterMeta(p *jmapi.PhotoDetail) (int, string) {
	if p.SeriesID != "" && p.SeriesID != "0" {
		s.albumMu.Lock()
		e, ok := s.albums[p.SeriesID]
		s.albumMu.Unlock()
		if ok {
			for _, c := range albumChapters(e.album) {
				if c.ID == p.ID {
					return c.Index, c.Title
				}
			}
		}
	}
	return 1, p.Name
}

func parseU64(s string) uint64 {
	n, _ := strconv.ParseUint(strings.TrimSpace(s), 10, 64)
	return n
}

func (s *Server) handlePhoto(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path
	var id string
	switch {
	case strings.HasPrefix(path, "/api/chapter/"):
		id = lastSegment(strings.TrimPrefix(path, "/api/chapter/"))
	default:
		id = lastSegment(strings.TrimPrefix(path, "/api/photo/"))
	}
	if id == "" {
		apiError(w, 400, "缺少章节 ID")
		return
	}

	p, imgs, err := s.photoDetail(id)
	if err != nil {
		apiError(w, 502, "取章节失败："+err.Error())
		return
	}

	albumID := p.SeriesID
	if albumID == "" || albumID == "0" {
		albumID = p.ID
	}
	index, title := s.chapterMeta(p)
	scramble := parseU64(p.ScrambleID)
	aid := parseU64(p.ID)

	images := make([]map[string]any, 0, len(imgs))
	for i, im := range imgs {
		images = append(images, map[string]any{
			"page":     i,
			"url":      "/img/" + p.ID + "/" + strconv.Itoa(i),
			"jpeg_url": "", // 服务端不解码，没有 JPEG 兜底
			// 服务端直接把分段数算好 —— 客户端不用再猜文件名
			"seg":      segNum(scramble, aid, im.FileName),
			"filename": im.FileName,
		})
	}

	writeJSON(w, 200, map[string]any{
		"ok":          true,
		"id":          p.ID,
		"title":       title,
		"indextitle":  "",
		"index":       strconv.Itoa(index),
		"album_id":    albumID,
		"page_count":  len(images),
		"page_base":   0,
		"scramble_id": p.ScrambleID,
		"images":      images,
	})
}

// ---------------------------------------------------------------- 图片

func (s *Server) handleImage(w http.ResponseWriter, r *http.Request) {
	rest := strings.Trim(strings.TrimPrefix(r.URL.Path, "/img/"), "/")
	parts := strings.Split(rest, "/")
	if len(parts) != 2 {
		http.Error(w, "图片地址不完整", 404)
		return
	}
	photoID := parts[0]
	page, err := strconv.Atoi(parts[1])
	if err != nil || page < 0 {
		http.Error(w, "页码不是数字", 404)
		return
	}

	_, imgs, err := s.photoDetail(photoID)
	if err != nil {
		http.Error(w, "章节不存在："+err.Error(), 502)
		return
	}
	// 越界返回 404 —— 客户端靠这个状态码判断「本章结束」。
	if page >= len(imgs) {
		http.Error(w, "页码超出范围", 404)
		return
	}

	// force=jpeg 是老客户端的兼容参数。本地服务端不解码，转不了；
	// 原图本来就是 JPEG 的话照常发。
	if force := strings.ToLower(r.URL.Query().Get("force")); (force == "jpeg" || force == "jpg") &&
		!strings.EqualFold(imgs[page].Suffix, ".jpg") && !strings.EqualFold(imgs[page].Suffix, ".jpeg") {
		http.Error(w, "本地服务端不解码，没有 JPEG 转码", 404)
		return
	}

	img := imgs[page]
	path := s.cache.path(photoID, img.Index, img.Suffix)

	if !exists(path) {
		unlock := s.cache.withLock(path)
		defer unlock()
		if !exists(path) {
			jc, err := s.upstream()
			if err != nil {
				http.Error(w, err.Error(), 503)
				return
			}
			data, err := jc.DownloadImage(img.DownloadURL)
			if err != nil {
				http.Error(w, "下载失败："+err.Error(), 502)
				return
			}
			if len(data) == 0 {
				http.Error(w, "下载到的字节为空", 502)
				return
			}
			if err := s.cache.save(path, data); err != nil {
				// 存不下不影响这一次访问，但下次还得重下
				http.Error(w, "写缓存失败："+err.Error(), 502)
				return
			}
			s.cache.prune()
		}
	}

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

func contentTypeOf(path string, data []byte) string {
	switch strings.ToLower(filepath.Ext(path)) {
	case ".webp":
		return "image/webp"
	case ".jpg", ".jpeg":
		return "image/jpeg"
	case ".png":
		return "image/png"
	case ".gif":
		return "image/gif"
	case ".bmp":
		return "image/bmp"
	}
	// 后缀不认识的，看魔数
	switch {
	case len(data) > 12 && string(data[0:4]) == "RIFF" && string(data[8:12]) == "WEBP":
		return "image/webp"
	case len(data) > 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF:
		return "image/jpeg"
	case len(data) > 8 && string(data[0:8]) == "\x89PNG\r\n\x1a\n":
		return "image/png"
	case len(data) > 6 && (string(data[0:6]) == "GIF87a" || string(data[0:6]) == "GIF89a"):
		return "image/gif"
	}
	return "application/octet-stream"
}


// ---------------------------------------------------------------- 封面

// 1x1 透明 GIF：封面拿不到时用它占位。
// 客户端对空/坏封面本来就会显示空框，但返回 404 会让它对每一本都重试 ——
// 一个占位图能让它安静地放弃。
var placeholderGIF = []byte{
	0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00,
	0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00,
	0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
	0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
}

func (s *Server) handleCover(w http.ResponseWriter, r *http.Request) {
	id := lastSegment(strings.TrimPrefix(r.URL.Path, "/cover/"))
	if id == "" {
		http.Error(w, "缺少本子 ID", 404)
		return
	}
	path := s.cache.coverPath(id)

	if !exists(path) {
		unlock := s.cache.withLock(path)
		defer unlock()
		if !exists(path) {
			var data []byte
			var err error
			if jc, e := s.upstream(); e == nil {
				data, err = jc.DownloadAlbumCover(id)
			} else {
				err = e
			}
			if err != nil || len(data) == 0 {
				w.Header().Set("Content-Type", "image/gif")
				w.Header().Set("Cache-Control", "public, max-age=3600")
				_, _ = w.Write(placeholderGIF)
				return
			}
			if err := s.cache.save(path, data); err != nil {
				// 写不进去也照发，只是下次还得重下
				_ = err
			}
		}
	}

	data, err := os.ReadFile(path)
	if err != nil {
		w.Header().Set("Content-Type", "image/gif")
		_, _ = w.Write(placeholderGIF)
		return
	}
	w.Header().Set("Content-Type", contentTypeOf(path, data))
	w.Header().Set("Content-Length", strconv.Itoa(len(data)))
	w.Header().Set("Cache-Control", "public, max-age=86400")
	_, _ = w.Write(data)
}

// ---------------------------------------------------------------- 缓存

// handleCache 管理磁盘缓存的上限与内容。
//
//	GET  /api/cache                  → 用量统计
//	POST /api/cache {max_mb: 1024}   → 改上限（0 = 不限制），**立刻**清理
//	POST /api/cache {clear: true}    → 清空
//
// 客户端在设置页里改「缓存上限」时调它；启动连上服务端时也会推一次，
// 保证 `cache_limit.txt` 里的值和进程里的值一致（launch.sh 只在启动那一刻
// 读那个文件，用户运行中改了的话必须靠这条接口同步）。
func (s *Server) handleCache(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		st := s.cache.Stats()
		writeJSON(w, 200, map[string]any{
			"ok":         true,
			"used_bytes": st.UsedBytes,
			"used_mb":    st.UsedMB,
			"limit_mb":   st.LimitMB,
			"files":      st.Files,
		})

	case http.MethodPost:
		var body struct {
			MaxMB *int64 `json:"max_mb"`
			Clear bool   `json:"clear"`
		}
		data, _ := io.ReadAll(io.LimitReader(r.Body, 1<<16))
		if len(data) > 0 {
			if err := json.Unmarshal(data, &body); err != nil {
				apiError(w, 400, "请求体不是合法 JSON")
				return
			}
		}

		var st Stats
		var err error
		if body.Clear {
			st, err = s.cache.Clear()
			if err != nil {
				log.Printf("清空缓存时出错：%v", err)
			}
		}
		if body.MaxMB != nil {
			st = s.cache.SetMaxMB(*body.MaxMB)
		}
		if !body.Clear && body.MaxMB == nil {
			st = s.cache.Stats()
		}

		writeJSON(w, 200, map[string]any{
			"ok":         true,
			"used_bytes": st.UsedBytes,
			"used_mb":    st.UsedMB,
			"limit_mb":   st.LimitMB,
			"files":      st.Files,
		})

	default:
		apiError(w, 405, "不支持的方法")
	}
}

// ---------------------------------------------------------------- 观看记录

func (s *Server) handleHistory(w http.ResponseWriter, r *http.Request) {
	switch r.Method {
	case http.MethodGet:
		writeJSON(w, 200, map[string]any{"ok": true, "items": s.hist.List()})

	case http.MethodPost:
		var body struct {
			ID    string `json:"id"`
			Title string `json:"title"`
		}
		data, _ := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		if err := json.Unmarshal(data, &body); err != nil {
			apiError(w, 400, "请求体不是合法 JSON")
			return
		}
		if strings.TrimSpace(body.ID) == "" {
			apiError(w, 400, "缺少 id")
			return
		}
		title := strings.TrimSpace(body.Title)
		if title == "" {
			// 客户端没给标题就自己查一个；查不到就用 id，不阻断记录
			if a, err := s.albumDetail(body.ID); err == nil {
				title = a.Name
			} else {
				title = body.ID
			}
		}
		items := s.hist.Put(body.ID, title)
		writeJSON(w, 200, map[string]any{"ok": true, "items": items})

	case http.MethodDelete:
		// ⚠️ 两种 DELETE 靠**前缀是否匹配**来区分，不能用 `lastSegment` 判空。
		//
		//    踩过：原来写的是
		//        if id := lastSegment(strings.TrimPrefix(path, "/api/history/")); id != "" {
		//            Delete(id)
		//        }
		//        Clear()
		//    而 `TrimPrefix("/api/history", "/api/history/")` 前缀不匹配、原样返回，
		//    于是 `lastSegment("/api/history")` 得到 **"history"** ——
		//    非空！结果「清空全部」被当成「删掉 id 为 history 的那条」，
		//    表现是请求成功返回 200，但列表一条没少。
		rest := strings.TrimPrefix(r.URL.Path, "/api/history/")
		if rest == r.URL.Path {
			// 没匹配到 "/api/history/" → 这是 DELETE /api/history，清空全部
			writeJSON(w, 200, map[string]any{"ok": true, "items": s.hist.Clear()})
			return
		}
		if id := lastSegment(rest); id != "" {
			writeJSON(w, 200, map[string]any{"ok": true, "items": s.hist.Delete(id)})
			return
		}
		writeJSON(w, 200, map[string]any{"ok": true, "items": s.hist.Clear()})

	default:
		apiError(w, 405, "不支持的方法")
	}
}

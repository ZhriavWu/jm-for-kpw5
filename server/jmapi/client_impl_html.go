package jmapi

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"sort"
	"strconv"
	"strings"
)

type htmlImpl struct{ c *Client }

func newHTMLImpl(c *Client) clientImpl { return &htmlImpl{c: c} }

var (
	reB64HTML              = regexp.MustCompile(`const html = base64DecodeUtf8\("(.*?)"\)`)
	reAlbumName            = regexp.MustCompile(`id="book-name"[^>]*?>([\s\S]*?)<`)
	reAlbumDescription     = regexp.MustCompile(`叙述：([\s\S]*?)</h2>`)
	reAlbumPageCount       = regexp.MustCompile(`<span class="pagecount">.*?:(\d+)</span>`)
	reAlbumLikes           = regexp.MustCompile(`<span id="albim_likes_\d+">(.*?)</span>`)
	reAlbumViews           = regexp.MustCompile(`<span>(.*?)</span>\n *<span>(次觀看|观看次数|次观看次数)</span>`)
	reAlbumCommentCount    = regexp.MustCompile(`<div class="badge"[^>]*?id="total_video_comments">(\d+)</div>`)
	reAlbumAuthorsBlock    = regexp.MustCompile(`<span itemprop="author" data-type="author">([\s\S]*?)</span>`)
	reAlbumWorksBlock      = regexp.MustCompile(`<span itemprop="author" data-type="works">([\s\S]*?)</span>`)
	reAlbumActorsBlock     = regexp.MustCompile(`<span itemprop="author" data-type="actor">([\s\S]*?)</span>`)
	reAlbumTagsBlock       = regexp.MustCompile(`<span itemprop="genre" data-type="tags">([\s\S]*?)</span>`)
	reTagA                 = regexp.MustCompile(`<a[^>]*?>\s*(\S*)\s*</a>`)
	reAlbumPhotoIDs        = regexp.MustCompile(`/photo/(\d+)/`)
	reAlbumScrambleID      = regexp.MustCompile(`var scramble_id = (\d+);`)
	reAlbumPubDate         = regexp.MustCompile(`>上架日期 : (.*?)</span>`)
	reAlbumUpdateDate      = regexp.MustCompile(`>更新日期 : (.*?)</span>`)
	reAlbumEpisodeList     = regexp.MustCompile(`data-album="(\d+)"[^>]*>[\s\S]*?第(\d+)[话話]([\s\S]*?)<[\s\S]*?>`)

	rePhotoScrambleID  = regexp.MustCompile(`var scramble_id = (\d+);`)
	rePhotoSeriesID    = regexp.MustCompile(`var series_id = (\d+);`)
	rePhotoSort        = regexp.MustCompile(`var sort = (\d+);`)
	rePhotoPageArr     = regexp.MustCompile(`var page_arr = (.*?);`)
	rePhotoName        = regexp.MustCompile(`<title>([\s\S]*?)\|.*</title>`)
	rePhotoTags        = regexp.MustCompile(`<meta name="keywords"[\s\S]*?content="(.*?)"`)
	rePhotoDataOriginalDomain = regexp.MustCompile(`src="https://(.*?)/media/albums/blank`)
	rePhotoDataOriginal0      = regexp.MustCompile(`data-original="(.*?)"[^>]*?id="album_photo[^>]*?data-page="0"`)

	reSearchShortenFor         = regexp.MustCompile(`<div class="well well-sm">([\s\S]*)<div class="row">`)
	reSearchAlbumInfoList      = regexp.MustCompile(`<a href="/album/(\d+)/[\s\S]*?title="(.*?)"([\s\S]*?)<div class="title-truncate tags .*>([\s\S]*?)</div>`)
	reCategoryAlbumInfoList    = regexp.MustCompile(`<a href="/album/(\d+)/[^>]*>[^>]*?title="(.*?)"[^>]*>[ \n]*</a>[ \n]*<div class="label-loveicon">([\s\S]*?)<div class="clearfix">`)
	reSearchTags               = regexp.MustCompile(`<a[^>]*?>(.*?)</a>`)
	reSearchTotal              = regexp.MustCompile(`class="text-white">(\d+)</span> A漫\.`)
	reSearchError              = regexp.MustCompile(`<fieldset>\n<legend>(.*?)</legend>\n<div class=.*?>\n(.*?)\n</div>\n</fieldset>`)

	reFavoriteContent = regexp.MustCompile(`<div id="favorites_album_[^>]*?>[\s\S]*?<a href="/album/(\d+)/[^"]*">[\s\S]*?<div class="video-title title-truncate">([^<]*?)</div>`)
	reFavoriteTotal   = regexp.MustCompile(` : (\d+)[^/]*/\D*(\d+)`)
)

func (h *htmlImpl) requestHTML(method string, path string, q url.Values, form url.Values) ([]byte, string, error) {
	if h.c == nil {
		return nil, "", fmt.Errorf("nil client")
	}
	if len(h.c.cfg.Domains) == 0 {
		return nil, "", fmt.Errorf("empty domains")
	}
	if !strings.HasPrefix(path, "/") && !strings.HasPrefix(path, "http") {
		return nil, "", fmt.Errorf("invalid path: %s", path)
	}

	domains := append([]string{}, h.c.cfg.Domains...)
	var lastErr error
	for _, d := range domains {
		for i := 0; i <= h.c.cfg.RetryTimes; i++ {
			u := path
			if strings.HasPrefix(path, "/") {
				u = (&url.URL{Scheme: "https", Host: d, Path: path}).String()
			}
			if q != nil && len(q) != 0 {
				if strings.Contains(u, "?") {
					u = u + "&" + q.Encode()
				} else {
					u = u + "?" + q.Encode()
				}
			}

			var body io.Reader
			if method != http.MethodGet && form != nil {
				body = strings.NewReader(form.Encode())
			}

			req, err := http.NewRequest(method, u, body)
			if err != nil {
				lastErr = err
				continue
			}
			req.Header.Set("user-agent", DefaultHTMLUserAgent)
			req.Header.Set("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
			req.Header.Set("accept-language", "zh-CN,zh;q=0.9,en;q=0.8")
			if method != http.MethodGet && form != nil {
				req.Header.Set("content-type", "application/x-www-form-urlencoded")
			}
			for k, v := range h.c.cfg.Headers {
				req.Header.Set(k, v)
			}
			for k, v := range h.c.cfg.Cookies {
				req.AddCookie(&http.Cookie{Name: k, Value: v})
			}

			resp, err := h.c.httpClient.Do(req)
			if err != nil {
				lastErr = err
				continue
			}
			for _, ck := range resp.Cookies() {
				if h.c.cfg.Cookies == nil {
					h.c.cfg.Cookies = map[string]string{}
				}
				h.c.cfg.Cookies[ck.Name] = ck.Value
			}
			finalURL := ""
			if resp.Request != nil && resp.Request.URL != nil {
				finalURL = resp.Request.URL.String()
			}

			data, err := readHTTPBody(resp)
			_ = resp.Body.Close()
			if err != nil {
				lastErr = err
				continue
			}
			if resp.StatusCode >= 500 {
				lastErr = fmt.Errorf("html server error http %d", resp.StatusCode)
				continue
			}
			if resp.StatusCode >= 400 {
				lastErr = fmt.Errorf("http %d: %s", resp.StatusCode, strings.TrimSpace(string(data)))
				continue
			}
			return data, finalURL, nil
		}
	}
	return nil, "", lastErr
}

func (h *htmlImpl) parseMaybeB64HTML(html string) string {
	m := reB64HTML.FindStringSubmatch(html)
	if len(m) < 2 {
		return html
	}
	raw, err := base64.StdEncoding.DecodeString(m[1])
	if err != nil {
		return html
	}
	return string(raw)
}

func findAllUnique(re *regexp.Regexp, s string) []string {
	if re == nil {
		return nil
	}
	m := re.FindAllStringSubmatch(s, -1)
	if len(m) == 0 {
		return nil
	}
	seen := map[string]bool{}
	out := make([]string, 0, len(m))
	for _, item := range m {
		if len(item) < 2 {
			continue
		}
		v := strings.TrimSpace(item[1])
		if v == "" || seen[v] {
			continue
		}
		seen[v] = true
		out = append(out, v)
	}
	sort.Strings(out)
	return out
}

func (h *htmlImpl) parseTagBlock(block string) []string {
	if strings.TrimSpace(block) == "" {
		return nil
	}
	tags := reTagA.FindAllStringSubmatch(block, -1)
	if len(tags) == 0 {
		return nil
	}
	seen := map[string]bool{}
	out := make([]string, 0, len(tags))
	for _, t := range tags {
		if len(t) < 2 {
			continue
		}
		v := strings.TrimSpace(t[1])
		if v == "" || seen[v] {
			continue
		}
		seen[v] = true
		out = append(out, v)
	}
	return out
}

func (h *htmlImpl) GetAlbumDetail(albumID string) (*AlbumDetail, error) {
	body, _, err := h.requestHTML(http.MethodGet, "/album/"+albumID+"/", nil, nil)
	if err != nil {
		return nil, err
	}
	html := h.parseMaybeB64HTML(string(body))

	ret := &AlbumDetail{ID: albumID}
	if m := reAlbumName.FindStringSubmatch(html); len(m) >= 2 {
		ret.Name = strings.TrimSpace(htmlUnescape(m[1]))
	}
	if m := reAlbumDescription.FindStringSubmatch(html); len(m) >= 2 {
		ret.Description = strings.TrimSpace(htmlUnescape(stripTags(m[1])))
	}
	if m := reAlbumPageCount.FindStringSubmatch(html); len(m) >= 2 {
		ret.PageCount, _ = strconv.Atoi(m[1])
	}
	if m := reAlbumLikes.FindStringSubmatch(html); len(m) >= 2 {
		ret.Likes = strings.TrimSpace(stripTags(m[1]))
	}
	if m := reAlbumViews.FindStringSubmatch(html); len(m) >= 2 {
		ret.Views = strings.TrimSpace(stripTags(m[1]))
	}
	if m := reAlbumCommentCount.FindStringSubmatch(html); len(m) >= 2 {
		ret.CommentCount, _ = strconv.Atoi(strings.TrimSpace(m[1]))
	}
	if m := reAlbumScrambleID.FindStringSubmatch(html); len(m) >= 2 {
		ret.ScrambleID = strings.TrimSpace(m[1])
	}
	if m := reAlbumPubDate.FindStringSubmatch(html); len(m) >= 2 {
		ret.PubDate = strings.TrimSpace(stripTags(m[1]))
	}
	if m := reAlbumUpdateDate.FindStringSubmatch(html); len(m) >= 2 {
		ret.UpdateDate = strings.TrimSpace(stripTags(m[1]))
	}

	if m := reAlbumAuthorsBlock.FindStringSubmatch(html); len(m) >= 2 {
		ret.Author = h.parseTagBlock(m[1])
	}
	if m := reAlbumWorksBlock.FindStringSubmatch(html); len(m) >= 2 {
		ret.Works = h.parseTagBlock(m[1])
	}
	if m := reAlbumActorsBlock.FindStringSubmatch(html); len(m) >= 2 {
		ret.Actors = h.parseTagBlock(m[1])
	}
	if m := reAlbumTagsBlock.FindStringSubmatch(html); len(m) >= 2 {
		ret.Tags = h.parseTagBlock(m[1])
	}

	ret.EpisodeIDs = findAllUnique(reAlbumPhotoIDs, html)
	// 结构化 episode_list（尽量解析；解析不到也不影响 EpisodeIDs）
	if matches := reAlbumEpisodeList.FindAllStringSubmatch(html, -1); len(matches) != 0 {
		ret.EpisodeList = make([]Episode, 0, len(matches))
		seen := map[string]bool{}
		for _, m := range matches {
			if len(m) < 4 {
				continue
			}
			pid := strings.TrimSpace(m[1])
			if pid == "" || seen[pid] {
				continue
			}
			seen[pid] = true
			idx, _ := strconv.Atoi(strings.TrimSpace(m[2]))
			title := strings.TrimSpace(htmlUnescape(stripTags(m[3])))
			ret.EpisodeList = append(ret.EpisodeList, Episode{
				PhotoID: pid,
				Index:   idx,
				Title:   title,
			})
		}
	}
	return ret, nil
}

func (h *htmlImpl) GetPhotoDetail(photoID string, fetchAlbum bool, fetchScrambleID bool) (*PhotoDetail, error) {
	body, _, err := h.requestHTML(http.MethodGet, "/photo/"+photoID+"/", nil, nil)
	if err != nil {
		return nil, err
	}
	html := string(body)

	ret := &PhotoDetail{ID: photoID}
	if m := rePhotoName.FindStringSubmatch(html); len(m) >= 2 {
		ret.Name = strings.TrimSpace(htmlUnescape(m[1]))
	}
	if m := rePhotoSeriesID.FindStringSubmatch(html); len(m) >= 2 {
		ret.AlbumID = strings.TrimSpace(m[1])
		ret.SeriesID = ret.AlbumID
	}
	if m := rePhotoSort.FindStringSubmatch(html); len(m) >= 2 {
		ret.Sort, _ = strconv.Atoi(strings.TrimSpace(m[1]))
	}
	if fetchScrambleID {
		if m := rePhotoScrambleID.FindStringSubmatch(html); len(m) >= 2 {
			ret.ScrambleID = strings.TrimSpace(m[1])
		}
	}
	if m := rePhotoTags.FindStringSubmatch(html); len(m) >= 2 {
		raw := strings.TrimSpace(htmlUnescape(stripTags(m[1])))
		if raw != "" {
			// html 端通常是逗号分隔
			parts := strings.Split(raw, ",")
			out := make([]string, 0, len(parts))
			for _, p := range parts {
				p = strings.TrimSpace(p)
				if p != "" {
					out = append(out, p)
				}
			}
			ret.Tags = out
		}
	}
	if m := rePhotoDataOriginalDomain.FindStringSubmatch(html); len(m) >= 2 {
		ret.DataOriginalDomain = strings.TrimSpace(m[1])
	}
	if m := rePhotoDataOriginal0.FindStringSubmatch(html); len(m) >= 2 {
		ret.DataOriginal0 = strings.TrimSpace(htmlUnescape(m[1]))
		if idx := strings.LastIndex(ret.DataOriginal0, "?"); idx >= 0 && idx+1 < len(ret.DataOriginal0) {
			ret.DataOriginalQuery = strings.TrimSpace(ret.DataOriginal0[idx+1:])
		}
	}
	if m := rePhotoPageArr.FindStringSubmatch(html); len(m) >= 2 {
		raw := strings.TrimSpace(m[1])
		var arr []string
		if err := json.Unmarshal([]byte(raw), &arr); err != nil {
			// 一些页面可能是单引号数组，做一次宽松替换
			raw2 := strings.ReplaceAll(raw, "'", "\"")
			_ = json.Unmarshal([]byte(raw2), &arr)
		}
		ret.PageArr = arr
	}

	if fetchAlbum && ret.AlbumID != "" {
		alb, err := h.GetAlbumDetail(ret.AlbumID)
		if err == nil {
			ret.FromAlbum = alb
			if len(ret.Tags) == 0 && alb != nil && len(alb.Tags) != 0 {
				ret.Tags = append([]string{}, alb.Tags...)
			}
			_ = ret.EnsureAuthor("")
		}
	}
	return ret, nil
}

func (h *htmlImpl) CheckPhoto(photo *PhotoDetail) error {
	if photo == nil {
		return fmt.Errorf("photo is nil")
	}
	if photo.AlbumID == "" || len(photo.PageArr) == 0 {
		newPhoto, err := h.GetPhotoDetail(photo.ID, true, true)
		if err != nil {
			return err
		}
		*photo = *newPhoto
	}
	return nil
}

func (h *htmlImpl) GetScrambleID(photoID string) (string, error) {
	photo, err := h.GetPhotoDetail(photoID, false, true)
	if err != nil {
		return "", err
	}
	if strings.TrimSpace(photo.ScrambleID) == "" {
		return "", fmt.Errorf("scramble_id not found")
	}
	return photo.ScrambleID, nil
}

func (h *htmlImpl) Search(searchQuery string, page int, mainTag int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	if page <= 0 {
		page = 1
	}
	base := "/search/photos"
	if strings.TrimSpace(category) != "" && category != CategoryAll && category != "0" {
		if strings.TrimSpace(subCategory) != "" {
			base = fmt.Sprintf("%s/%s/sub/%s", base, category, subCategory)
		} else {
			base = fmt.Sprintf("%s/%s", base, category)
		}
	}
	q := url.Values{}
	q.Set("main_tag", strconv.Itoa(mainTag))
	q.Set("search_query", searchQuery)
	q.Set("page", strconv.Itoa(page))
	q.Set("o", orderBy)
	q.Set("t", timeRange)

	body, finalURL, err := h.requestHTML(http.MethodGet, base, q, nil)
	if err != nil {
		return nil, err
	}
	html := string(body)

	// 搜索车号会重定向到 album 页
	if strings.Contains(finalURL, "/album/") {
		aid := firstMatchGroup(regexp.MustCompile(`/album/(\d+)/`), finalURL)
		if aid != "" {
			alb, err := h.GetAlbumDetail(aid)
			if err != nil {
				return nil, err
			}
			return &SearchResult{
				Total: 1,
				Items: []AlbumListItem{{ID: alb.ID, Name: alb.Name, TagList: alb.Tags}},
			}, nil
		}
	}

	return parseHTMLSearchPage(html)
}

func (h *htmlImpl) CategoriesFilter(page int, timeRange, category, orderBy, subCategory string) (*SearchResult, error) {
	if page <= 0 {
		page = 1
	}
	base := "/albums"
	if strings.TrimSpace(category) != "" && category != CategoryAll && category != "0" {
		if strings.TrimSpace(subCategory) != "" {
			base = fmt.Sprintf("%s/%s/sub/%s", base, category, subCategory)
		} else {
			base = fmt.Sprintf("%s/%s", base, category)
		}
	}

	q := url.Values{}
	q.Set("page", strconv.Itoa(page))
	q.Set("o", orderBy)
	q.Set("t", timeRange)

	body, _, err := h.requestHTML(http.MethodGet, base, q, nil)
	if err != nil {
		return nil, err
	}
	return parseHTMLCategoryPage(string(body))
}

func (h *htmlImpl) Setting() (map[string]any, error) {
	return nil, fmt.Errorf("Setting not supported by html client")
}

func (h *htmlImpl) Login(username, password string) (map[string]any, error) {
	form := url.Values{}
	form.Set("username", username)
	form.Set("password", password)
	form.Set("id_remember", "on")
	form.Set("login_remember", "on")
	form.Set("submit_login", "")

	_, _, err := h.requestHTML(http.MethodPost, "/login", nil, form)
	if err != nil {
		return nil, err
	}
	return map[string]any{"username": username}, nil
}

func (h *htmlImpl) FavoriteFolder(page int, orderBy, folderID, username string) (*FavoriteResult, error) {
	if page <= 0 {
		page = 1
	}
	if strings.TrimSpace(username) == "" {
		return nil, fmt.Errorf("username is required for html favorite")
	}
	if strings.TrimSpace(folderID) == "" {
		folderID = "0"
	}

	q := url.Values{}
	q.Set("page", strconv.Itoa(page))
	q.Set("o", orderBy)
	q.Set("folder", folderID)

	body, _, err := h.requestHTML(http.MethodGet, "/user/"+url.PathEscape(username)+"/favorite/albums", q, nil)
	if err != nil {
		return nil, err
	}
	html := string(body)

	total := 0
	if m := reFavoriteTotal.FindStringSubmatch(html); len(m) >= 3 {
		// 形如 " : 1 / 12"：优先取第二个分组(总数)
		total, _ = strconv.Atoi(strings.TrimSpace(m[2]))
		if total == 0 {
			total, _ = strconv.Atoi(strings.TrimSpace(m[1]))
		}
	}

	items := []AlbumListItem{}
	matches := reFavoriteContent.FindAllStringSubmatch(html, -1)
	for _, m := range matches {
		if len(m) < 3 {
			continue
		}
		items = append(items, AlbumListItem{ID: strings.TrimSpace(m[1]), Name: strings.TrimSpace(htmlUnescape(m[2]))})
	}
	return &FavoriteResult{Total: total, Items: items}, nil
}

func (h *htmlImpl) AddFavoriteAlbum(albumID, folderID string) (map[string]any, error) {
	q := url.Values{}
	q.Set("album_id", albumID)
	if strings.TrimSpace(folderID) == "" {
		folderID = "0"
	}
	q.Set("fid", folderID)

	body, _, err := h.requestHTML(http.MethodGet, "/ajax/favorite_album", q, nil)
	if err != nil {
		return nil, err
	}
	var obj map[string]any
	if err := json.Unmarshal(body, &obj); err != nil {
		return nil, fmt.Errorf("favorite response not json: %w", err)
	}
	if st, ok := obj["status"].(float64); ok && int(st) != 1 {
		return nil, fmt.Errorf("favorite failed: %v", obj["msg"])
	}
	return obj, nil
}

func (h *htmlImpl) AlbumComment(videoID, comment, originator, status, commentID string) (map[string]any, error) {
	form := url.Values{}
	form.Set("video_id", videoID)
	form.Set("comment", comment)
	form.Set("originator", originator)
	if status == "" {
		status = "true"
	}
	form.Set("status", status)
	if strings.TrimSpace(commentID) != "" {
		form.Del("status")
		form.Set("comment_id", commentID)
		form.Set("is_reply", "1")
		form.Set("forum_subject", "1")
	}

	body, _, err := h.requestHTML(http.MethodPost, "/ajax/album_comment", nil, form)
	if err != nil {
		return nil, err
	}
	var obj map[string]any
	if err := json.Unmarshal(body, &obj); err != nil {
		return nil, fmt.Errorf("comment response not json: %w", err)
	}
	return obj, nil
}

func (h *htmlImpl) DownloadImage(imgURL string) ([]byte, error) {
	// 复用现有下载逻辑（仅 UA 不同无所谓）
	return h.c.apiDownloadImage(imgURL)
}

func (h *htmlImpl) DownloadByImageDetail(photoID, imageName string) ([]byte, error) {
	photo, err := h.GetPhotoDetail(photoID, false, true)
	if err != nil {
		return nil, err
	}
	if imageName == "" {
		if len(photo.PageArr) == 0 {
			return nil, fmt.Errorf("photo has no images")
		}
		imageName = photo.PageArr[0]
	}
	imgURL, err := photo.ImageURL(imageName)
	if err != nil {
		return nil, err
	}
	return h.DownloadImage(imgURL)
}

func (h *htmlImpl) DownloadAlbumCover(albumID string) ([]byte, error) {
	return h.c.apiDownloadAlbumCover(albumID)
}

func (h *htmlImpl) AutoUpdateDomains() error {
	// HTML 域名不走 API 域名服务器更新；保持用户配置即可
	return nil
}

func parseHTMLSearchPage(html string) (*SearchResult, error) {
	if m := reSearchError.FindStringSubmatch(html); len(m) >= 3 {
		return nil, fmt.Errorf("%s: %s", strings.TrimSpace(stripTags(m[1])), strings.TrimSpace(stripTags(m[2])))
	}
	if m := reSearchShortenFor.FindStringSubmatch(html); len(m) >= 2 {
		html = m[1]
	}

	total := 0
	if m := reSearchTotal.FindStringSubmatch(html); len(m) >= 2 {
		total, _ = strconv.Atoi(strings.TrimSpace(m[1]))
	}

	items := []AlbumListItem{}
	matches := reSearchAlbumInfoList.FindAllStringSubmatch(html, -1)
	for _, m := range matches {
		if len(m) < 5 {
			continue
		}
		tags := reSearchTags.FindAllStringSubmatch(m[4], -1)
		tagList := make([]string, 0, len(tags))
		for _, t := range tags {
			if len(t) >= 2 {
				tagList = append(tagList, strings.TrimSpace(htmlUnescape(stripTags(t[1]))))
			}
		}
		items = append(items, AlbumListItem{
			ID:      strings.TrimSpace(m[1]),
			Name:    strings.TrimSpace(htmlUnescape(m[2])),
			TagList: compactStringSlice(tagList),
		})
	}
	return &SearchResult{Total: total, Items: items}, nil
}

func parseHTMLCategoryPage(html string) (*SearchResult, error) {
	total := 0
	if m := reSearchTotal.FindStringSubmatch(html); len(m) >= 2 {
		total, _ = strconv.Atoi(strings.TrimSpace(m[1]))
	}
	items := []AlbumListItem{}
	matches := reCategoryAlbumInfoList.FindAllStringSubmatch(html, -1)
	for _, m := range matches {
		if len(m) < 4 {
			continue
		}
		tags := reSearchTags.FindAllStringSubmatch(m[3], -1)
		tagList := make([]string, 0, len(tags))
		for _, t := range tags {
			if len(t) >= 2 {
				tagList = append(tagList, strings.TrimSpace(htmlUnescape(stripTags(t[1]))))
			}
		}
		items = append(items, AlbumListItem{
			ID:      strings.TrimSpace(m[1]),
			Name:    strings.TrimSpace(htmlUnescape(m[2])),
			TagList: compactStringSlice(tagList),
		})
	}
	return &SearchResult{Total: total, Items: items}, nil
}

func compactStringSlice(in []string) []string {
	seen := map[string]bool{}
	out := make([]string, 0, len(in))
	for _, s := range in {
		s = strings.TrimSpace(s)
		if s == "" || seen[s] {
			continue
		}
		seen[s] = true
		out = append(out, s)
	}
	return out
}

func firstMatchGroup(re *regexp.Regexp, s string) string {
	if re == nil {
		return ""
	}
	m := re.FindStringSubmatch(s)
	if len(m) >= 2 {
		return m[1]
	}
	return ""
}

// 非严格 HTML 处理：足够支撑解析关键字段
func stripTags(s string) string {
	out := regexp.MustCompile(`<[^>]+>`).ReplaceAllString(s, "")
	return strings.TrimSpace(out)
}

func htmlUnescape(s string) string {
	replacer := strings.NewReplacer(
		"&amp;", "&",
		"&lt;", "<",
		"&gt;", ">",
		"&quot;", `"`,
		"&#39;", "'",
		"&nbsp;", " ",
	)
	return replacer.Replace(s)
}


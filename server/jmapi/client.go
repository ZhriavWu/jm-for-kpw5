package jmapi

import (
	"bytes"
	"compress/gzip"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

type Client struct {
	cfg        Config
	httpClient *http.Client
	fixedTS    string
	impl       clientImpl
}

type apiEnvelope struct {
	Code     int             `json:"code"`
	Data     json.RawMessage `json:"data"`
	ErrorMsg string          `json:"errorMsg"`
}

type clientImpl interface {
	GetAlbumDetail(albumID string) (*AlbumDetail, error)
	GetPhotoDetail(photoID string, fetchAlbum bool, fetchScrambleID bool) (*PhotoDetail, error)
	CheckPhoto(photo *PhotoDetail) error
	GetScrambleID(photoID string) (string, error)

	Search(searchQuery string, page int, mainTag int, orderBy, timeRange, category, subCategory string) (*SearchResult, error)
	CategoriesFilter(page int, timeRange, category, orderBy, subCategory string) (*SearchResult, error)

	Setting() (map[string]any, error)
	Login(username, password string) (map[string]any, error)
	FavoriteFolder(page int, orderBy, folderID, username string) (*FavoriteResult, error)
	AddFavoriteAlbum(albumID, folderID string) (map[string]any, error)
	AlbumComment(videoID, comment, originator, status, commentID string) (map[string]any, error)

	DownloadImage(imgURL string) ([]byte, error)
	DownloadByImageDetail(photoID, imageName string) ([]byte, error)
	DownloadAlbumCover(albumID string) ([]byte, error)

	AutoUpdateDomains() error
}

type apiImpl struct{ c *Client }

func newAPIImpl(c *Client) clientImpl { return &apiImpl{c: c} }

func NewClient(cfg Config) *Client {
	if cfg.ClientType == "" {
		cfg.ClientType = ClientTypeAPI
	}
	if cfg.Timeout <= 0 {
		cfg.Timeout = 25 * time.Second
	}
	if cfg.RetryTimes <= 0 {
		cfg.RetryTimes = 2
	}
	if cfg.AppVersion == "" {
		cfg.AppVersion = DefaultAppVersion
	}
	if len(cfg.Domains) == 0 {
		if cfg.ClientType == ClientTypeHTML {
			cfg.Domains = append([]string{}, DefaultHTMLDomains...)
		} else {
			cfg.Domains = append([]string{}, DefaultAPIDomains...)
		}
	}

	c := &Client{
		cfg: cfg,
		httpClient: &http.Client{
			Timeout: cfg.Timeout,
		},
	}

	if cfg.UseFixedTimestamp {
		c.fixedTS = strconv.FormatInt(time.Now().Unix(), 10)
	}

	// 双客户端统一抽象：对外仍是 *Client，对内按 ClientType 路由
	if cfg.ClientType == ClientTypeHTML {
		c.impl = newHTMLImpl(c)
	} else {
		c.impl = newAPIImpl(c)
	}

	if cfg.AutoUpdateHost && cfg.ClientType == ClientTypeAPI {
		_ = c.AutoUpdateDomains()
	}

	if cfg.AutoEnsureCookies && cfg.ClientType == ClientTypeAPI {
		_, _ = c.Setting()
	}

	return c
}

func (c *Client) SetDomains(domains []string) {
	c.cfg.Domains = append([]string{}, domains...)
}

func (c *Client) Domains() []string {
	return append([]string{}, c.cfg.Domains...)
}

func (c *Client) UpdateCookies(cookies map[string]string) {
	if c.cfg.Cookies == nil {
		c.cfg.Cookies = map[string]string{}
	}
	for k, v := range cookies {
		c.cfg.Cookies[k] = v
	}
}

// ---------------- 统一门面：对外 API 不变 ----------------

func (c *Client) GetAlbumDetail(albumID string) (*AlbumDetail, error) {
	return c.impl.GetAlbumDetail(albumID)
}

func (c *Client) GetPhotoDetail(photoID string, fetchAlbum bool, fetchScrambleID bool) (*PhotoDetail, error) {
	return c.impl.GetPhotoDetail(photoID, fetchAlbum, fetchScrambleID)
}

func (c *Client) CheckPhoto(photo *PhotoDetail) error {
	return c.impl.CheckPhoto(photo)
}

func (c *Client) GetScrambleID(photoID string) (string, error) {
	return c.impl.GetScrambleID(photoID)
}

func (c *Client) Search(searchQuery string, page int, mainTag int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.impl.Search(searchQuery, page, mainTag, orderBy, timeRange, category, subCategory)
}

func (c *Client) SearchSite(searchQuery string, page int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.Search(searchQuery, page, 0, orderBy, timeRange, category, subCategory)
}

func (c *Client) SearchWork(searchQuery string, page int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.Search(searchQuery, page, 1, orderBy, timeRange, category, subCategory)
}

func (c *Client) SearchAuthor(searchQuery string, page int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.Search(searchQuery, page, 2, orderBy, timeRange, category, subCategory)
}

func (c *Client) SearchTag(searchQuery string, page int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.Search(searchQuery, page, 3, orderBy, timeRange, category, subCategory)
}

func (c *Client) SearchActor(searchQuery string, page int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return c.Search(searchQuery, page, 4, orderBy, timeRange, category, subCategory)
}

func (c *Client) CategoriesFilter(page int, timeRange, category, orderBy, subCategory string) (*SearchResult, error) {
	return c.impl.CategoriesFilter(page, timeRange, category, orderBy, subCategory)
}

func (c *Client) MonthRanking(page int, category string) (*SearchResult, error) {
	if category == "" {
		category = CategoryAll
	}
	return c.CategoriesFilter(page, TimeMonth, category, OrderByView, "")
}

func (c *Client) WeekRanking(page int, category string) (*SearchResult, error) {
	if category == "" {
		category = CategoryAll
	}
	return c.CategoriesFilter(page, TimeWeek, category, OrderByView, "")
}

func (c *Client) DayRanking(page int, category string) (*SearchResult, error) {
	if category == "" {
		category = CategoryAll
	}
	return c.CategoriesFilter(page, TimeToday, category, OrderByView, "")
}

func (c *Client) Setting() (map[string]any, error) {
	return c.impl.Setting()
}

func (c *Client) Login(username, password string) (map[string]any, error) {
	return c.impl.Login(username, password)
}

func (c *Client) FavoriteFolder(page int, orderBy, folderID, username string) (*FavoriteResult, error) {
	return c.impl.FavoriteFolder(page, orderBy, folderID, username)
}

func (c *Client) AddFavoriteAlbum(albumID, folderID string) (map[string]any, error) {
	return c.impl.AddFavoriteAlbum(albumID, folderID)
}

func (c *Client) AlbumComment(videoID, comment, originator, status, commentID string) (map[string]any, error) {
	return c.impl.AlbumComment(videoID, comment, originator, status, commentID)
}

func (c *Client) DownloadImage(imgURL string) ([]byte, error) {
	return c.impl.DownloadImage(imgURL)
}

func (c *Client) DownloadByImageDetail(photoID, imageName string) ([]byte, error) {
	return c.impl.DownloadByImageDetail(photoID, imageName)
}

func (c *Client) DownloadAlbumCover(albumID string) ([]byte, error) {
	return c.impl.DownloadAlbumCover(albumID)
}

func (c *Client) AutoUpdateDomains() error {
	return c.impl.AutoUpdateDomains()
}

func (c *Client) requestAPI(method, path string, query map[string]string, body map[string]any, forceSecret string) (map[string]any, error) {
	if len(c.cfg.Domains) == 0 {
		return nil, fmt.Errorf("empty domains")
	}

	tryDomains := append([]string{}, c.cfg.Domains...)

	requestWithDomains := func(domains []string) (map[string]any, error) {
		var lastErr error
		for _, d := range domains {
			for i := 0; i <= c.cfg.RetryTimes; i++ {
				res, err := c.requestOnce(method, d, path, query, body, forceSecret)
				if err == nil {
					return res, nil
				}
				lastErr = err
			}
		}
		return nil, lastErr
	}

	res, err := requestWithDomains(tryDomains)
	if err == nil {
		return res, nil
	}

	// 自动更新得到的域名可能短时不稳定，失败后回退到内置默认域名再试一次
	if c.cfg.ClientType == ClientTypeAPI {
		fallback := append([]string{}, DefaultAPIDomains...)
		if len(fallback) != 0 {
			res2, err2 := requestWithDomains(fallback)
			if err2 == nil {
				c.cfg.Domains = fallback
				return res2, nil
			}
			return nil, fmt.Errorf("all retries failed (updated domains then fallback domains): updated_err=%v, fallback_err=%w", err, err2)
		}
	}

	return nil, fmt.Errorf("all retries failed: %w", err)
}

func (c *Client) requestOnce(method, domain, path string, query map[string]string, body map[string]any, forceSecret string) (map[string]any, error) {
	ts := c.decideTS()
	secret := forceSecret
	if secret == "" {
		secret = AppTokenSecret
	}
	token, tokenParam := tokenAndTokenParam(ts, c.cfg.AppVersion, secret)

	u := url.URL{Scheme: "https", Host: domain, Path: path}
	if len(query) != 0 {
		q := u.Query()
		for k, v := range query {
			q.Set(k, v)
		}
		u.RawQuery = q.Encode()
	}

	var reqBody io.Reader
	if method != http.MethodGet && body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return nil, err
		}
		reqBody = bytes.NewReader(b)
	}

	req, err := http.NewRequest(method, u.String(), reqBody)
	if err != nil {
		return nil, err
	}

	req.Header.Set("user-agent", DefaultMobileUserAgent)
	req.Header.Set("accept-encoding", "gzip, deflate")
	req.Header.Set("token", token)
	req.Header.Set("tokenparam", tokenParam)
	if method != http.MethodGet && body != nil {
		req.Header.Set("content-type", "application/json")
	}
	for k, v := range c.cfg.Headers {
		req.Header.Set(k, v)
	}
	for k, v := range c.cfg.Cookies {
		req.AddCookie(&http.Cookie{Name: k, Value: v})
	}

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	for _, ck := range resp.Cookies() {
		if c.cfg.Cookies == nil {
			c.cfg.Cookies = map[string]string{}
		}
		c.cfg.Cookies[ck.Name] = ck.Value
	}

	payload, err := readHTTPBody(resp)
	if err != nil {
		return nil, err
	}

	if resp.StatusCode >= 500 {
		return nil, fmt.Errorf("api server error http %d: %s", resp.StatusCode, strings.TrimSpace(string(payload)))
	}
	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("http %d: %s", resp.StatusCode, strings.TrimSpace(string(payload)))
	}

	var env apiEnvelope
	if err := json.Unmarshal(payload, &env); err != nil {
		return nil, fmt.Errorf("json parse failed: %w", err)
	}
	if env.Code != 200 {
		return nil, fmt.Errorf("api code=%d error=%s", env.Code, env.ErrorMsg)
	}

	decodedRaw, err := c.decodeEnvelopeData(env.Data, ts)
	if err != nil {
		return nil, err
	}

	var obj map[string]any
	if err := json.Unmarshal(decodedRaw, &obj); err == nil {
		return obj, nil
	}
	var list []any
	if err := json.Unmarshal(decodedRaw, &list); err == nil {
		return map[string]any{"list": list}, nil
	}
	return nil, fmt.Errorf("decoded data is neither object nor list")
}

func (c *Client) decodeEnvelopeData(raw json.RawMessage, ts string) ([]byte, error) {
	if len(raw) == 0 || string(raw) == "null" {
		return []byte("{}"), nil
	}

	var encrypted string
	if err := json.Unmarshal(raw, &encrypted); err == nil {
		return decodeRespData(encrypted, ts, AppDataSecret)
	}

	return raw, nil
}

func (c *Client) decideTS() string {
	if c.cfg.UseFixedTimestamp {
		if c.fixedTS == "" {
			c.fixedTS = strconv.FormatInt(time.Now().Unix(), 10)
		}
		return c.fixedTS
	}
	return strconv.FormatInt(time.Now().Unix(), 10)
}

func readHTTPBody(resp *http.Response) ([]byte, error) {
	if resp == nil || resp.Body == nil {
		return nil, fmt.Errorf("empty http response body")
	}

	enc := strings.ToLower(strings.TrimSpace(resp.Header.Get("Content-Encoding")))
	if strings.Contains(enc, "gzip") {
		zr, err := gzip.NewReader(resp.Body)
		if err != nil {
			return nil, fmt.Errorf("gzip reader init failed: %w", err)
		}
		defer zr.Close()
		return io.ReadAll(zr)
	}

	data, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}

	// 部分服务端不会回 Content-Encoding，但正文是 gzip（二进制头 1f 8b）
	if len(data) >= 2 && data[0] == 0x1f && data[1] == 0x8b {
		zr, err := gzip.NewReader(bytes.NewReader(data))
		if err != nil {
			return nil, fmt.Errorf("gzip magic detected but unzip failed: %w", err)
		}
		defer zr.Close()
		return io.ReadAll(zr)
	}

	return data, nil
}

func toStr(v any) string {
	// [本地补丁] 上游 bug：nil 会走 default 分支，fmt.Sprintf("%v", nil)
	// 返回字符串 "<nil>"，于是"字段缺失"变成"字段值是 <nil>"，
	// 导致 ImageURL 拼出 https://<nil>/... 无法下载，兜底域名也永远不触发。
	if v == nil {
		return ""
	}
	switch t := v.(type) {
	case string:
		return t
	case float64:
		return strconv.FormatInt(int64(t), 10)
	case int:
		return strconv.Itoa(t)
	case int64:
		return strconv.FormatInt(t, 10)
	default:
		s := fmt.Sprintf("%v", t)
		if s == "<nil>" {
			return ""
		}
		return s
	}
}

func toInt(v any) int {
	s, err := strconv.Atoi(toStr(v))
	if err != nil {
		return 0
	}
	return s
}

func toStrSlice(v any) []string {
	arr, ok := v.([]any)
	if !ok {
		return nil
	}
	out := make([]string, 0, len(arr))
	for _, item := range arr {
		out = append(out, toStr(item))
	}
	return out
}

func (c *Client) parseSearchResult(obj map[string]any) *SearchResult {
	ret := &SearchResult{Raw: obj}
	ret.Total = toInt(obj["total"])

	arr, ok := obj["content"].([]any)
	if !ok {
		arr, _ = obj["list"].([]any)
	}
	for _, item := range arr {
		m, ok := item.(map[string]any)
		if !ok {
			continue
		}
		ret.Items = append(ret.Items, AlbumListItem{
			ID:       toStr(m["id"]),
			Author:   toStr(m["author"]),
			Description: toStr(m["description"]),
			Image:    toStr(m["image"]),
			Name:     toStr(m["name"]),
			Label:    toStr(m["label"]),
			Category: toStr(m["category"]),
			CategorySub: toStr(m["category_sub"]),
			TagList:  toStrSlice(m["tag_list"]),
			Raw:      m,
		})
	}
	return ret
}

// ---------------- API 实现（供 apiImpl 调用） ----------------

func (c *Client) apiGetAlbumDetail(albumID string) (*AlbumDetail, error) {
	obj, err := c.requestAPI(http.MethodGet, "/album", map[string]string{"id": albumID}, nil, "")
	if err != nil {
		return nil, err
	}

	ret := &AlbumDetail{ID: albumID, Raw: obj}
	ret.ID = toStr(obj["id"])
	ret.Name = toStr(obj["name"])
	ret.ScrambleID = toStr(obj["scramble_id"])
	ret.Description = toStr(obj["description"])
	ret.Author = toStrSlice(obj["author"])
	ret.Tags = toStrSlice(obj["tags"])
	ret.Works = toStrSlice(obj["works"])
	ret.Actors = toStrSlice(obj["actors"])
	ret.PageCount = toInt(obj["page_count"])
	ret.CommentCount = toInt(obj["comment_total"])
	ret.PubDate = toStr(obj["pub_date"])
	ret.UpdateDate = toStr(obj["update_date"])
	ret.Likes = toStr(obj["likes"])
	ret.Views = toStr(obj["views"])

	// episode_list: [{id|photo_id, sort|index, name|title, pub_date}, ...]
	if raw, ok := obj["episode_list"].([]any); ok && len(raw) != 0 {
		ret.EpisodeList = make([]Episode, 0, len(raw))
		for _, item := range raw {
			m, ok := item.(map[string]any)
			if !ok {
				continue
			}
			ep := Episode{
				PhotoID: toStr(m["photo_id"]),
				Index:   toInt(m["index"]),
				Title:   toStr(m["title"]),
				PubDate: toStr(m["pub_date"]),
			}
			if ep.PhotoID == "" {
				ep.PhotoID = toStr(m["id"])
			}
			if ep.Index == 0 {
				ep.Index = toInt(m["sort"])
			}
			if ep.Title == "" {
				ep.Title = toStr(m["name"])
			}
			ret.EpisodeList = append(ret.EpisodeList, ep)
		}
	}

	// related_list（尽量适配多种返回结构）
	if raw, ok := obj["related_list"].([]any); ok && len(raw) != 0 {
		ret.RelatedList = make([]AlbumListItem, 0, len(raw))
		for _, item := range raw {
			m, ok := item.(map[string]any)
			if !ok {
				continue
			}
			ret.RelatedList = append(ret.RelatedList, AlbumListItem{
				ID:          toStr(m["id"]),
				Name:        toStr(m["name"]),
				Author:      toStr(m["author"]),
				Description: toStr(m["description"]),
				Image:       toStr(m["image"]),
				Label:       toStr(m["label"]),
				Category:    toStr(m["category"]),
				CategorySub: toStr(m["category_sub"]),
				TagList:     toStrSlice(m["tag_list"]),
				Raw:         m,
			})
		}
	}
	return ret, nil
}

func (c *Client) apiGetPhotoDetail(photoID string, fetchAlbum bool, fetchScrambleID bool) (*PhotoDetail, error) {
	obj, err := c.requestAPI(http.MethodGet, "/chapter", map[string]string{"id": photoID}, nil, "")
	if err != nil {
		return nil, err
	}

	ret := &PhotoDetail{ID: photoID, Raw: obj}
	ret.ID = toStr(obj["id"])
	ret.Name = toStr(obj["name"])
	ret.AlbumID = toStr(obj["series_id"])
	ret.SeriesID = ret.AlbumID
	ret.Sort = toInt(obj["sort"])
	ret.PageArr = toStrSlice(obj["images"])
	ret.Tags = toStrSlice(obj["tags"])

	// API 端一般不返回 data-original 域名与 v 参数；这里保持空，由后续兜底生成 v=ts + 默认CDN
	// 若未来接口返回对应字段，可在这里补充映射：
	ret.DataOriginalDomain = toStr(obj["data_original_domain"])
	ret.DataOriginal0 = toStr(obj["data_original_0"])
	ret.DataOriginalQuery = toStr(obj["data_original_query"])

	// [本地补丁] /chapter 的响应里本身就带 scramble_id，
	// 直接取就行。上游只调 /chapter_view_template，那个接口拿不到时
	// ScrambleID 会是空字符串，随后 strconv.Atoi("") 得 0，
	// 分段数算错 → 解密出来的图是错位的（实测 MAD 14，正确应 <3）。
	if sid := toStr(obj["scramble_id"]); sid != "" {
		ret.ScrambleID = sid
	}
	if fetchScrambleID && ret.ScrambleID == "" {
		sid, err := c.apiGetScrambleID(photoID)
		if err == nil {
			ret.ScrambleID = sid
		}
	}
	if fetchAlbum && ret.AlbumID != "" {
		alb, err := c.apiGetAlbumDetail(ret.AlbumID)
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

func (c *Client) apiCheckPhoto(photo *PhotoDetail) error {
	if photo == nil {
		return fmt.Errorf("photo is nil")
	}
	if photo.AlbumID == "" || len(photo.PageArr) == 0 {
		newPhoto, err := c.apiGetPhotoDetail(photo.ID, true, true)
		if err != nil {
			return err
		}
		*photo = *newPhoto
	}
	return nil
}

// [本地补丁] 与 jmcomic 对齐的兜底值。
// jmcomic 的 fetch_scramble_id 是从 /chapter_view_template 的**响应文本**里
// 正则提取的；提取不到时它直接兜底成 220980（JmMagicConstants.SCRAMBLE_220980）。
// 上游 Go 版把这个接口当加密 JSON 解析，而它返回的是 HTML，
// 于是永远拿不到值 → ScrambleID 为空 → Atoi("")=0 → 分段数算错 → 解密错位。
const fallbackScrambleID = "220980"

func (c *Client) apiGetScrambleID(photoID string) (string, error) {
	obj, err := c.requestAPI(http.MethodGet, "/chapter_view_template", map[string]string{
		"id":            photoID,
		"mode":          "vertical",
		"page":          "0",
		"app_img_shunt": "1",
		"express":       "off",
		"v":             strconv.FormatInt(time.Now().Unix(), 10),
	}, nil, AppTokenSecret2)
	if err != nil {
		// 解析失败是常态（该接口返回 HTML），直接走兜底
		return fallbackScrambleID, nil
	}

	if sid := toStr(obj["scramble_id"]); sid != "" {
		return sid, nil
	}
	if sid := toStr(obj["id"]); sid != "" {
		return sid, nil
	}
	return fallbackScrambleID, nil
}

// ---------------- 搜索接口 ----------------

func (c *Client) apiSearch(searchQuery string, page int, mainTag int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	obj, err := c.requestAPI(http.MethodGet, "/search", map[string]string{
		"search_query": searchQuery,
		"page":         strconv.Itoa(page),
		"main_tag":     strconv.Itoa(mainTag),
		"o":            orderBy,
		"t":            timeRange,
	}, nil, "")
	if err != nil {
		return nil, err
	}
	return c.parseSearchResult(obj), nil
}

// ---------------- 分类/排行接口 ----------------

func (c *Client) apiCategoriesFilter(page int, timeRange, category, orderBy, subCategory string) (*SearchResult, error) {
	o := orderBy
	if timeRange != TimeAll && timeRange != "" {
		o = orderBy + "_" + timeRange
	}

	obj, err := c.requestAPI(http.MethodGet, "/categories/filter", map[string]string{
		"page":  strconv.Itoa(page),
		"order": "",
		"c":     category,
		"o":     o,
	}, nil, "")
	if err != nil {
		return nil, err
	}

	return c.parseSearchResult(obj), nil
}

// ---------------- 用户接口 ----------------

func (c *Client) apiSetting() (map[string]any, error) {
	return c.requestAPI(http.MethodGet, "/setting", nil, nil, "")
}

func (c *Client) apiLogin(username, password string) (map[string]any, error) {
	obj, err := c.requestAPI(http.MethodPost, "/login", nil, map[string]any{
		"username": username,
		"password": password,
	}, "")
	if err != nil {
		return nil, err
	}

	if s := toStr(obj["s"]); s != "" {
		c.UpdateCookies(map[string]string{"AVS": s})
	}
	return obj, nil
}

func (c *Client) apiFavoriteFolder(page int, orderBy, folderID, username string) (*FavoriteResult, error) {
	if page <= 0 {
		page = 1
	}
	if folderID == "" {
		folderID = "0"
	}

	obj, err := c.requestAPI(http.MethodGet, "/favorite", map[string]string{
		"page":      strconv.Itoa(page),
		"folder_id": folderID,
		"o":         orderBy,
	}, nil, "")
	if err != nil {
		return nil, err
	}

	ret := &FavoriteResult{Raw: obj}
	ret.Total = toInt(obj["total"])
	arr, _ := obj["list"].([]any)
	for _, item := range arr {
		m, ok := item.(map[string]any)
		if !ok {
			continue
		}
		ret.Items = append(ret.Items, AlbumListItem{ID: toStr(m["id"]), Name: toStr(m["name"])})
	}
	return ret, nil
}

func (c *Client) apiAddFavoriteAlbum(albumID, folderID string) (map[string]any, error) {
	return c.requestAPI(http.MethodPost, "/favorite", nil, map[string]any{"aid": albumID}, "")
}

func (c *Client) apiAlbumComment(videoID, comment, originator, status, commentID string) (map[string]any, error) {
	if status == "" {
		status = "true"
	}
	body := map[string]any{
		"video_id":   videoID,
		"comment":    comment,
		"originator": originator,
		"status":     status,
	}
	if commentID != "" {
		delete(body, "status")
		body["comment_id"] = commentID
		body["is_reply"] = 1
		body["forum_subject"] = 1
	}
	return c.requestAPI(http.MethodPost, "/ajax/album_comment", nil, body, "")
}

// ---------------- 图片接口 ----------------

func (c *Client) apiDownloadImage(imgURL string) ([]byte, error) {
	req, err := http.NewRequest(http.MethodGet, imgURL, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("user-agent", DefaultHTMLUserAgent)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("download image status %d", resp.StatusCode)
	}
	return io.ReadAll(resp.Body)
}

func (c *Client) apiDownloadByImageDetail(photoID, imageName string) ([]byte, error) {
	photo, err := c.apiGetPhotoDetail(photoID, false, true)
	if err != nil {
		return nil, err
	}
	if photo.ScrambleID == "" {
		photo.ScrambleID, _ = c.apiGetScrambleID(photoID)
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
	return c.apiDownloadImage(imgURL)
}

func (c *Client) apiDownloadAlbumCover(albumID string) ([]byte, error) {
	imgURL := fmt.Sprintf("https://cdn-msp.jmapiproxy1.cc/media/albums/%s_3x4.jpg", albumID)
	return c.apiDownloadImage(imgURL)
}

// ---------------- 域名相关接口 ----------------

func (c *Client) apiAutoUpdateDomains() error {
	for _, u := range DefaultAPIDomainServerURLs {
		serverList, err := c.reqAPIDomainServer(u)
		if err != nil || len(serverList) == 0 {
			continue
		}
		if len(c.cfg.Domains) > 0 {
			c.cfg.Domains = serverList
		}
		return nil
	}
	return fmt.Errorf("unable to auto update domains")
}

func (c *Client) reqAPIDomainServer(serverURL string) ([]string, error) {
	req, err := http.NewRequest(http.MethodGet, serverURL, nil)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	text := string(body)
	for len(text) > 0 && text[0] > 127 {
		text = text[1:]
	}

	decoded, err := decodeRespData(text, "", APIDomainServerSecret)
	if err != nil {
		return nil, err
	}

	var data map[string]any
	if err := json.Unmarshal(decoded, &data); err != nil {
		return nil, err
	}
	arr, ok := data["Server"].([]any)
	if !ok {
		return nil, fmt.Errorf("invalid domain server response")
	}

	out := make([]string, 0, len(arr))
	for _, item := range arr {
		out = append(out, toStr(item))
	}
	return out, nil
}

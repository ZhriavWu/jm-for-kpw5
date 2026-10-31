package jmapi

import (
	"fmt"
	"net/url"
	"path"
	"strconv"
	"strings"
	"time"
)

func (p *PhotoDetail) IsSingleAlbum() bool {
	// Python 版：series_id == 0 认为单章本子
	s := strings.TrimSpace(p.SeriesID)
	return s == "" || s == "0"
}

func (p *PhotoDetail) ResolvedAlbumID() string {
	if p.IsSingleAlbum() {
		return p.ID
	}
	// 兼容：有些实现把 series_id 放在 AlbumID
	if strings.TrimSpace(p.SeriesID) != "" {
		return p.SeriesID
	}
	return p.AlbumID
}

func (p *PhotoDetail) AlbumIndex() int {
	// Python 版：单章本子 sort=2 时语义上返回 1
	if p.IsSingleAlbum() && p.Sort == 2 {
		return 1
	}
	if p.Sort <= 0 {
		return 1
	}
	return p.Sort
}

func (p *PhotoDetail) IndexTitle() string {
	return fmt.Sprintf("第%d话 %s", p.AlbumIndex(), p.Name)
}

func (p *PhotoDetail) EnsureAuthor(defaultAuthor string) string {
	if strings.TrimSpace(p.Author) != "" {
		return p.Author
	}
	if p.FromAlbum != nil && len(p.FromAlbum.Author) > 0 && strings.TrimSpace(p.FromAlbum.Author[0]) != "" {
		p.Author = strings.TrimSpace(p.FromAlbum.Author[0])
		return p.Author
	}
	p.Author = strings.TrimSpace(defaultAuthor)
	return p.Author
}

func (p *PhotoDetail) EnsureDataOriginalQuery() string {
	if strings.TrimSpace(p.DataOriginalQuery) != "" {
		return p.DataOriginalQuery
	}
	if strings.TrimSpace(p.DataOriginal0) != "" {
		if idx := strings.LastIndex(p.DataOriginal0, "?"); idx >= 0 && idx+1 < len(p.DataOriginal0) {
			p.DataOriginalQuery = strings.TrimSpace(p.DataOriginal0[idx+1:])
			if p.DataOriginalQuery != "" {
				return p.DataOriginalQuery
			}
		}
	}
	// Python 版：拿不到就用当前时间戳兜底
	p.DataOriginalQuery = "v=" + strconv.FormatInt(time.Now().Unix(), 10)
	return p.DataOriginalQuery
}

func (p *PhotoDetail) ImageURL(imgName string) (string, error) {
	imgName = strings.TrimSpace(imgName)
	if imgName == "" {
		return "", fmt.Errorf("empty image name")
	}

	domain := strings.TrimSpace(p.DataOriginalDomain)
	if domain == "" {
		// 兼容旧实现：没有解析到 domain 时回退到固定 CDN
		domain = "cdn-msp.jmapiproxy1.cc"
	}

	u := url.URL{
		Scheme: "https",
		Host:   domain,
		Path:   path.Join("/media/photos", p.ID, imgName),
	}
	q := strings.TrimSpace(p.EnsureDataOriginalQuery())
	if q != "" {
		u.RawQuery = q
	}
	return u.String(), nil
}


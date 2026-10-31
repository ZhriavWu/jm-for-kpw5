package main

import (
	"fmt"
	"strconv"
	"sync"
	"time"

	"github.com/laoin114514/jmapi"
)

type searchEntry struct {
	items []jmapi.AlbumListItem
	total int
	at    time.Time
}

// SearchCache 缓存「上游 API 的一页」（80 条）。
//
// 为什么要缓存上游页而不是显示页：显示页是按 6 条切出来的，翻 6 页才往前
// 挪一个上游页；不缓存的话每翻一页都要重新请求一次源站。
type SearchCache struct {
	mu sync.Mutex
	m  map[string]*searchEntry
}

const (
	searchTTL      = 10 * time.Minute
	searchCacheMax = 64
)

func NewSearchCache() *SearchCache {
	return &SearchCache{m: make(map[string]*searchEntry)}
}

func (c *SearchCache) get(key string) (*searchEntry, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	e, ok := c.m[key]
	if !ok || time.Since(e.at) > searchTTL {
		return nil, false
	}
	return e, true
}

func (c *SearchCache) put(key string, e *searchEntry) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if len(c.m) >= searchCacheMax {
		// 简单粗暴：满了就清空。搜索缓存丢了最多多一次网络往返，
		// 不值得为它写一套 LRU。
		c.m = make(map[string]*searchEntry)
	}
	c.m[key] = e
}

// apiPage 取上游第 page 页（1 基，每页 80 条）。
func (s *Server) apiPage(keyword, order string, page int) ([]jmapi.AlbumListItem, int, error) {
	key := keyword + "\x00" + order + "\x00" + strconv.Itoa(page)
	if e, ok := s.search.get(key); ok {
		return e.items, e.total, nil
	}

	jc, err := s.upstream()
	if err != nil {
		return nil, 0, err
	}
	s.jmu.Lock()
	res, err := jc.Search(keyword, page, 0, order, "", "", "")
	s.jmu.Unlock()
	if err != nil {
		return nil, 0, err
	}
	e := &searchEntry{items: res.Items, total: res.Total, at: time.Now()}
	s.search.put(key, e)
	return e.items, e.total, nil
}

// searchSlice 把上游的 80 条/页切成客户端要的 6 条/页。
//
// 和 jm_kindle_raw.py 的 search_slice 同一套逻辑：
//   - 显示页越界 → 收敛到最后一页重取；
//   - 正好跨在 80 条边界上 → 补取下一条上游页。
//
// 返回 (本页条目, 实际页号, 总页数, 总条数)。总页数 0 表示上游没给总数，
// 这时客户端只显示「当前页」。
func (s *Server) searchSlice(keyword, order string, page int) ([]jmapi.AlbumListItem, int, int, int, error) {
	if page < 1 {
		page = 1
	}
	locate := func(p int) (start, apiPage, offset int) {
		start = (p - 1) * perPage
		return start, start/apiPageSize + 1, start % apiPageSize
	}

	_, apiIdx, offset := locate(page)
	items, total, err := s.apiPage(keyword, order, apiIdx)
	if err != nil {
		return nil, page, 0, 0, err
	}

	lastDisplay := 0
	if total > 0 {
		lastDisplay = (total + perPage - 1) / perPage
		if lastDisplay < 1 {
			lastDisplay = 1
		}
		if page > lastDisplay {
			page = lastDisplay
			_, apiIdx, offset = locate(page)
			items, total, err = s.apiPage(keyword, order, apiIdx)
			if err != nil {
				return nil, page, 0, 0, err
			}
		}
	}

	end := offset + perPage
	if end > len(items) {
		end = len(items)
	}
	pageItems := append([]jmapi.AlbumListItem{}, items[offset:end]...)

	// 跨上游页边界：补取下一条上游页
	if len(pageItems) < perPage && len(items) >= apiPageSize {
		more, _, err := s.apiPage(keyword, order, apiIdx+1)
		if err != nil {
			return nil, page, 0, 0, fmt.Errorf("补取下一页失败：%w", err)
		}
		need := perPage - len(pageItems)
		if need > len(more) {
			need = len(more)
		}
		pageItems = append(pageItems, more[:need]...)
	}

	return pageItems, page, lastDisplay, total, nil
}

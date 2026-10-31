package main

import (
	"errors"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// Cache 是磁盘缓存：`<dir>/<photo_id>/<page><suffix>` 和 `<dir>/_covers/<album>.jpg`。
//
// 只存**源站原图**，不做任何加工（加工意味着重新编码，有损、变慢、还占更多地方）。
type Cache struct {
	dir string
	max int64 // 字节；0 = 不限制

	mu     sync.Mutex
	locks  map[string]*keyLock
	total  int64
	pruned time.Time
}

// keyLock 是「同一把钥匙只允许一个下载」的互斥量。
//
// 为什么需要：客户端翻页时可能并发请求同一页（预读 + 手动翻），
// 两个请求同时发现文件不存在就会各下一份，浪费流量还可能写坏文件。
type keyLock struct {
	mu   sync.Mutex
	refs int
}

func NewCache(dir string, max int64) (*Cache, error) {
	if err := os.MkdirAll(filepath.Join(dir, "_covers"), 0o755); err != nil {
		return nil, err
	}
	return &Cache{dir: dir, max: max, locks: make(map[string]*keyLock)}, nil
}

// withLock 拿到某个 key 的独占权，返回解锁函数。
func (c *Cache) withLock(key string) func() {
	c.mu.Lock()
	l, ok := c.locks[key]
	if !ok {
		l = &keyLock{}
		c.locks[key] = l
	}
	l.refs++
	c.mu.Unlock()

	l.mu.Lock()
	return func() {
		l.mu.Unlock()
		c.mu.Lock()
		l.refs--
		if l.refs == 0 {
			delete(c.locks, key)
		}
		c.mu.Unlock()
	}
}

func (c *Cache) path(photoID string, page int, suffix string) string {
	return filepath.Join(c.dir, photoID, pad4(page)+suffix)
}

func (c *Cache) coverPath(albumID string) string {
	return filepath.Join(c.dir, "_covers", albumID+".jpg")
}

func pad4(n int) string {
	s := strconv.Itoa(n)
	for len(s) < 4 {
		s = "0" + s
	}
	return s
}

// exists 判断文件是否可用。空文件、写了一半的文件都当不存在。
func exists(path string) bool {
	st, err := os.Stat(path)
	return err == nil && st.Size() > 0
}

// save 原子写：先写 .part 再改名。
//
// Kindle 是 FAT32，直接覆写时断电会留下半截文件 —— 而半截 webp 会让
// 客户端解码失败，表现是「这一页永远加载不出来」。
func (c *Cache) save(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	tmp := path + ".part"
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// prune 按「最久没访问」删到上限之内。返回删了几个文件。
//
// 只在启动时和写入后偶尔调用 —— 每次写都全盘扫一遍在 Kindle 上太慢。
func (c *Cache) prune() int {
	if c.max <= 0 {
		return 0
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if time.Since(c.pruned) < 10*time.Minute {
		return 0
	}
	c.pruned = time.Now()

	type fileInfo struct {
		path string
		size int64
		at   time.Time
	}
	var files []fileInfo
	var total int64

	_ = filepath.Walk(c.dir, func(path string, info os.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() {
			return nil
		}
		if strings.HasSuffix(path, ".part") {
			return nil
		}
		files = append(files, fileInfo{path, info.Size(), info.ModTime()})
		total += info.Size()
		return nil
	})

	if total <= c.max {
		c.total = total
		return 0
	}

	// 最久没访问的排前面。读文件会更新 atime，但 FAT32 上 atime 不可靠，
	// 所以按 mtime（= 下载时间）算；顺序错一点没关系，目标是「别撑爆盘」。
	sort.Slice(files, func(i, j int) bool { return files[i].at.Before(files[j].at) })

	removed := 0
	for _, f := range files {
		if total <= c.max {
			break
		}
		if err := os.Remove(f.path); err == nil {
			total -= f.size
			removed++
		}
	}
	c.total = total
	if removed > 0 {
		log.Printf("缓存清理：删了 %d 个文件，现在 %.1f MB", removed, float64(total)/1e6)
	}
	return removed
}

var errEmptyDownload = errors.New("下载到的字节为空")

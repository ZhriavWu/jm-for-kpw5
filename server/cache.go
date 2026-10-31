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
//
// # 上限
//
// `max` 是**字节数**，0 表示不限制。客户端可以在设置里改（存 `cache_limit.txt`，
// 启动时由 launch.sh 传给本进程的 `-cache-max-mb`，运行中还能通过
// `POST /api/cache` 改）。超上限时按「最久没访问」删到上限以内。
type Cache struct {
	dir string

	mu     sync.Mutex
	max    int64
	total  int64
	files  int
	pruned time.Time

	locks map[string]*keyLock
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
	c := &Cache{dir: dir, max: max, locks: make(map[string]*keyLock)}
	// 启动时先把实际用量数出来 —— 否则「已用」会一直显示 0，
	// 直到第一次 prune 才变成真值。
	c.recount()
	return c, nil
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
	if err := os.Rename(tmp, path); err != nil {
		return err
	}

	c.mu.Lock()
	c.total += int64(len(data))
	c.files++
	c.mu.Unlock()
	return nil
}

// Stats 是给客户端看的用量快照。
type Stats struct {
	UsedBytes int64 `json:"used_bytes"`
	UsedMB    int64 `json:"used_mb"`
	LimitMB   int64 `json:"limit_mb"` // 0 = 不限制
	Files     int   `json:"files"`
}

func (c *Cache) statsLocked() Stats {
	limitMB := c.max / (1024 * 1024)
	if c.max <= 0 {
		limitMB = 0
	}
	return Stats{
		UsedBytes: c.total,
		UsedMB:    c.total / (1024 * 1024),
		LimitMB:   limitMB,
		Files:     c.files,
	}
}

// Stats 返回当前用量。
func (c *Cache) Stats() Stats {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.statsLocked()
}

// SetMaxMB 改上限并**立刻**执行一次清理。
//
// 立刻清理是关键：用户把上限从 2 GB 调到 500 MB 时，他期望马上看到磁盘
// 空出来，而不是「等下一次写缓存时才慢慢删」—— 那可能要等好几分钟。
func (c *Cache) SetMaxMB(mb int64) Stats {
	c.mu.Lock()
	if mb <= 0 {
		c.max = 0
	} else {
		c.max = mb * 1024 * 1024
	}
	c.pruned = time.Time{} // 清掉节流时间戳，让 prune 立刻跑
	c.mu.Unlock()

	c.prune()
	return c.Stats()
}

// Clear 删掉整个缓存目录里的文件（保留目录本身）。
func (c *Cache) Clear() (Stats, error) {
	var failed int
	_ = filepath.Walk(c.dir, func(path string, info os.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() {
			return nil
		}
		if err := os.Remove(path); err != nil {
			failed++
		}
		return nil
	})

	c.mu.Lock()
	c.total = 0
	c.files = 0
	c.mu.Unlock()

	if failed > 0 {
		return c.Stats(), errors.New("部分文件删不掉（可能正被占用）")
	}
	return c.Stats(), nil
}

// recount 从头统计一次目录用量。启动时调用。
func (c *Cache) recount() {
	var total int64
	var files int
	_ = filepath.Walk(c.dir, func(path string, info os.FileInfo, err error) error {
		if err != nil || info == nil || info.IsDir() {
			return nil
		}
		if strings.HasSuffix(path, ".part") {
			return nil
		}
		total += info.Size()
		files++
		return nil
	})
	c.mu.Lock()
	c.total = total
	c.files = files
	c.mu.Unlock()
}

// prune 按「最久没访问」删到上限之内。返回删了几个文件。
//
// 只在启动时和写入后偶尔调用 —— 每次写都全盘扫一遍在 Kindle 上太慢。
func (c *Cache) prune() int {
	c.mu.Lock()
	max := c.max
	if max <= 0 {
		c.mu.Unlock()
		return 0
	}
	if !c.pruned.IsZero() && time.Since(c.pruned) < 10*time.Minute {
		c.mu.Unlock()
		return 0
	}
	c.pruned = time.Now()
	c.mu.Unlock()

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

	removed := 0
	if total > max {
		// 最久没访问的排前面。读文件会更新 atime，但 FAT32 上 atime 不可靠，
		// 所以按 mtime（= 下载时间）算；顺序错一点没关系，目标是「别撑爆盘」。
		sort.Slice(files, func(i, j int) bool { return files[i].at.Before(files[j].at) })
		for _, f := range files {
			if total <= max {
				break
			}
			if err := os.Remove(f.path); err == nil {
				total -= f.size
				removed++
			}
		}
	}

	c.mu.Lock()
	c.total = total
	c.files = len(files) - removed
	c.mu.Unlock()

	if removed > 0 {
		// 上限小于 1 MB 时按 MB 显示会是「上限 0 MB」，很容易看成本 bug ——
		// 所以小上限单独用 KB 显示。
		if max < 1<<20 {
			log.Printf("缓存清理：删了 %d 个文件，现在 %.1f MB（上限 %d KB）",
				removed, float64(total)/1e6, max/1024)
		} else {
			log.Printf("缓存清理：删了 %d 个文件，现在 %.1f MB（上限 %d MB）",
				removed, float64(total)/1e6, max/(1024*1024))
		}
	}
	return removed
}

var errEmptyDownload = errors.New("下载到的字节为空")

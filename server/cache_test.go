package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func testCache(t *testing.T, maxBytes int64) *Cache {
	t.Helper()
	c, err := NewCache(filepath.Join(t.TempDir(), "cache"), maxBytes)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

// 保存文件后用量要准。
func TestCacheStats(t *testing.T) {
	c := testCache(t, 0)

	if st := c.Stats(); st.UsedBytes != 0 || st.Files != 0 {
		t.Fatalf("空缓存统计不对：%+v", st)
	}
	if err := c.save(c.path("111", 1, ".webp"), make([]byte, 1000)); err != nil {
		t.Fatal(err)
	}
	if err := c.save(c.coverPath("222"), make([]byte, 500)); err != nil {
		t.Fatal(err)
	}

	st := c.Stats()
	if st.UsedBytes != 1500 {
		t.Fatalf("用量应为 1500，实际 %d", st.UsedBytes)
	}
	if st.Files != 2 {
		t.Fatalf("文件数应为 2，实际 %d", st.Files)
	}
}

// ⭐ 超上限时按「最久没访问」删 —— 这是「防止缓存爆掉」的核心。
func TestCachePruneKeepsNewest(t *testing.T) {
	// 上限 2500 字节，写 5 个 1000 字节 → 必须删掉最早的 3 个
	c := testCache(t, 2500)

	for i := 1; i <= 5; i++ {
		p := c.path("111", i, ".webp")
		if err := c.save(p, make([]byte, 1000)); err != nil {
			t.Fatal(err)
		}
		// 用 mtime 排「谁最旧」，测试里要显式造出时间差
		old := time.Now().Add(time.Duration(i-5) * time.Hour)
		if err := os.Chtimes(p, old, old); err != nil {
			t.Fatal(err)
		}
	}

	c.prune()

	st := c.Stats()
	if st.UsedBytes > 2500 {
		t.Fatalf("清理后仍超上限：%d > 2500", st.UsedBytes)
	}
	// 最新的两个（4、5）必须还在
	for _, page := range []int{4, 5} {
		if !exists(c.path("111", page, ".webp")) {
			t.Fatalf("第 %d 页（较新）被误删了", page)
		}
	}
	// 最早的必须没了
	if exists(c.path("111", 1, ".webp")) {
		t.Fatal("最早的第 1 页应该被删掉")
	}
}

// 上限设成 0 = 不限制，一个字节都不该删。
func TestCacheNoLimit(t *testing.T) {
	c := testCache(t, 0)
	for i := 1; i <= 5; i++ {
		if err := c.save(c.path("111", i, ".webp"), make([]byte, 1000)); err != nil {
			t.Fatal(err)
		}
	}
	c.prune()
	if st := c.Stats(); st.Files != 5 {
		t.Fatalf("不限制时不该删文件，实际剩 %d 个", st.Files)
	}
}

// ⭐ 调小上限必须**立刻**清理，不能等下一次写缓存。
//
// 用户把 2 GB 调到 500 MB 时，他期望马上看到磁盘空出来。
func TestCacheSetMaxPrunesImmediately(t *testing.T) {
	c := testCache(t, 0) // 先不限制
	for i := 1; i <= 10; i++ {
		p := c.path("111", i, ".webp")
		if err := c.save(p, make([]byte, 1000)); err != nil {
			t.Fatal(err)
		}
		old := time.Now().Add(time.Duration(i-10) * time.Hour)
		_ = os.Chtimes(p, old, old)
	}
	if st := c.Stats(); st.UsedBytes != 10000 {
		t.Fatalf("准备阶段用量不对：%d", st.UsedBytes)
	}

	// 调到 3 KB（单位是 MB，所以这里手工换算成字节上限）
	c.mu.Lock()
	c.max = 3000
	c.pruned = time.Time{} // 让节流失效
	c.mu.Unlock()
	c.prune()

	if st := c.Stats(); st.UsedBytes > 3000 {
		t.Fatalf("调小上限后没清理：%d > 3000", st.UsedBytes)
	}
}

// 清空之后统计归零、文件全没。
func TestCacheClear(t *testing.T) {
	c := testCache(t, 0)
	for i := 1; i <= 3; i++ {
		if err := c.save(c.path("111", i, ".webp"), make([]byte, 100)); err != nil {
			t.Fatal(err)
		}
	}
	if err := c.save(c.coverPath("222"), make([]byte, 100)); err != nil {
		t.Fatal(err)
	}

	st, err := c.Clear()
	if err != nil {
		t.Fatalf("清空报错：%v", err)
	}
	if st.UsedBytes != 0 || st.Files != 0 {
		t.Fatalf("清空后统计没归零：%+v", st)
	}
	if exists(c.path("111", 1, ".webp")) || exists(c.coverPath("222")) {
		t.Fatal("清空后文件还在")
	}
}

// 启动时要**先数一遍**实际用量 —— 否则界面上的「已用」会一直是 0，
// 直到第一次 prune 才变成真值。
func TestCacheRecountOnStart(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "cache")
	if err := os.MkdirAll(filepath.Join(dir, "111"), 0o755); err != nil {
		t.Fatal(err)
	}
	// 直接摆两个文件，模拟「上次运行留下的缓存」
	if err := os.WriteFile(filepath.Join(dir, "111", "0001.webp"), make([]byte, 700), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "111", "0002.webp"), make([]byte, 300), 0o644); err != nil {
		t.Fatal(err)
	}

	c, err := NewCache(dir, 0)
	if err != nil {
		t.Fatal(err)
	}
	if st := c.Stats(); st.UsedBytes != 1000 || st.Files != 2 {
		t.Fatalf("启动时没数出已有用量：%+v", st)
	}
}

// 半截文件（.part）不该计入用量，也不该被当成缓存命中。
func TestCacheIgnoresPartFiles(t *testing.T) {
	c := testCache(t, 0)
	full := c.path("111", 1, ".webp")
	if err := c.save(full, make([]byte, 10)); err != nil {
		t.Fatal(err)
	}
	part := c.path("111", 2, ".webp") + ".part"
	if err := os.MkdirAll(filepath.Dir(part), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(part, make([]byte, 500), 0o644); err != nil {
		t.Fatal(err)
	}

	// 重新数一遍（模拟重启）：.part 不算
	c.recount()
	if st := c.Stats(); st.UsedBytes != 10 {
		t.Fatalf(".part 被算进用量了：%d", st.UsedBytes)
	}
}

package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

// 小包装，省得在测试里到处 import os
func osWriteFile(path string, data []byte) error {
	return os.WriteFile(path, data, 0o644)
}

// 回归测试：**锁不能自己等自己**。
//
// 踩过的 bug：`Put()` 先 `h.mu.Lock()`，最后又 `return h.List()`，
// 而 List() 里再 Lock 一次。sync.Mutex 不可重入 —— 那个请求永久挂住，
// 而且锁被占着，之后所有读历史的请求也一并堵死。
//
// 真机表现：第一次读历史正常（空表），一旦打开任何本子触发一次
// POST /api/history，历史页就再也拉不出来。服务端日志里看不出任何异常。
//
// 所以这里给每个会加锁的方法都套一个超时 —— 死锁的话测试会失败而不是挂住。
func TestHistoryNoDeadlock(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.json")
	h := NewHistory(path)

	done := make(chan struct{})
	go func() {
		defer close(done)
		h.Put("111", "第一本")
		h.List()
		h.Put("222", "第二本")
		h.Put("111", "第一本改名") // 重复 id 应该挪到最前，不新增
		h.Delete("222")
		h.List()
		h.Clear()
		h.List()
	}()

	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("历史记录操作卡住了 —— 大概率又出现了锁重入死锁")
	}
}

func TestHistoryOrderAndLimit(t *testing.T) {
	h := NewHistory(filepath.Join(t.TempDir(), "history.json"))

	h.Put("1", "甲")
	h.Put("2", "乙")
	h.Put("1", "甲改") // 重复：挪到最前，标题更新，条数不变

	items := h.List()
	if len(items) != 2 {
		t.Fatalf("条数应为 2，实际 %d：%+v", len(items), items)
	}
	if items[0].ID != "1" || items[0].Title != "甲改" {
		t.Fatalf("最新一条应在最前且标题已更新，实际 %+v", items[0])
	}

	for i := 0; i < historyMax+10; i++ {
		h.Put(string(rune('a'+i%26))+string(rune('0'+i/26)), "x")
	}
	if n := len(h.List()); n > historyMax {
		t.Fatalf("超过上限没被截断：%d > %d", n, historyMax)
	}
}

// 清空之后必须返回**空切片**：JSON 里是 `[]` 而不是 `null`。
// 客户端的 `items: Vec<HistoryItem>` 没有 serde default，收到 null 会解析失败。
func TestHistoryClearReturnsEmptySlice(t *testing.T) {
	h := NewHistory(filepath.Join(t.TempDir(), "history.json"))
	h.Put("1", "甲")

	items := h.Clear()
	if items == nil {
		t.Fatal("Clear 返回 nil —— 序列化成 null 会让客户端解析失败")
	}
	if len(items) != 0 {
		t.Fatalf("清空后应该是空表，实际 %+v", items)
	}

	data, _ := json.Marshal(struct {
		Items []HistoryItem `json:"items"`
	}{items})
	if string(data) != `{"items":[]}` {
		t.Fatalf("序列化结果应为 {\"items\":[]}，实际 %s", data)
	}
}

// 清空之后**磁盘上的文件**也必须是 `[]`，不是 `null`。
//
// `nil` 切片经 json.Marshal 得到 `null`；读回来虽然也是空表，但文件内容
// 让人看不懂，而且任何用严格 schema 读它的工具都会失败。
func TestHistoryClearWritesEmptyArrayOnDisk(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.json")
	h := NewHistory(path)
	h.Put("1", "甲")
	h.Clear()

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "[]" {
		t.Fatalf("清空后磁盘内容应为 []，实际 %q", string(data))
	}

	// 从没写过任何东西的新文件也不该是 null
	path2 := filepath.Join(t.TempDir(), "empty.json")
	h2 := NewHistory(path2)
	h2.Clear()
	if data, err := os.ReadFile(path2); err == nil && string(data) != "[]" {
		t.Fatalf("空表落盘应为 []，实际 %q", string(data))
	}
}

// 落盘之后换个实例还能读回来 —— 这是「重启后记录还在」的地基。
func TestHistoryPersists(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.json")
	a := NewHistory(path)
	a.Put("42", "某本子")

	b := NewHistory(path)
	items := b.List()
	if len(items) != 1 || items[0].ID != "42" || items[0].Title != "某本子" {
		t.Fatalf("重新加载后数据不对：%+v", items)
	}
}

// 坏文件不能让服务端起不来。
func TestHistoryCorruptFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "history.json")
	if err := osWriteFile(path, []byte("{ 这不是 JSON")); err != nil {
		t.Fatal(err)
	}
	h := NewHistory(path)
	if n := len(h.List()); n != 0 {
		t.Fatalf("坏文件应当成空表，实际 %d 条", n)
	}
	// 而且之后还能正常工作
	h.Put("1", "甲")
	if n := len(h.List()); n != 1 {
		t.Fatalf("坏文件之后写入失效：%d 条", n)
	}
}

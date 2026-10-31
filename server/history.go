package main

import (
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"sync"
)

// HistoryItem 和客户端的 `api::HistoryItem` 一一对应。
type HistoryItem struct {
	ID    string `json:"id"`
	Title string `json:"title"`
}

// History 是观看记录：一个 JSON 数组，最新的在前。
//
// 和 Python 版共用同一份格式（`[{"id","title"}, ...]`），所以从电脑换到
// Kindle 内置服务端时，老记录直接能用。
type History struct {
	path  string
	mu    sync.Mutex
	items []HistoryItem
	// 文件坏了 / 不存在时用空表，**绝不 panic** —— 记录丢了可以再攒，
// 服务端起不来就是整个应用都用不了。
}

const historyMax = 100

func NewHistory(path string) *History {
	// 初值就是空切片，不是 nil —— 这样「从没写过」和「清空过」落盘都是 `[]`。
	h := &History{path: path, items: []HistoryItem{}}
	if text, err := os.ReadFile(path); err == nil {
		var items []HistoryItem
		if json.Unmarshal(text, &items) == nil && items != nil {
			h.items = items
		}
	}
	return h
}

// listLocked 是**不带锁**的读拷贝，只给已经持锁的内部方法调用。
//
// ⚠️ 为什么必须单独拆一个：`sync.Mutex` **不可重入**。之前 `Put()` 是
//    「先 Lock、再 return h.List()」，而 List() 里又 Lock 一次 ——
//    同一把锁自己等自己，**永久死锁**，那个 HTTP 请求从此挂住，
//    连带着之后所有读历史的请求也全堵在锁上。
//
//    真机表现：客户端第一次读历史（空表）成功；一旦打开任何本子/章节
//    触发一次 POST /api/history，服务端就卡死，之后历史页再也拉不出来。
//    服务端日志里只会看到「收到退出信号」，看不出是锁住了。
func (h *History) listLocked() []HistoryItem {
	out := make([]HistoryItem, len(h.items))
	copy(out, h.items)
	return out
}

func (h *History) List() []HistoryItem {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.listLocked()
}

func (h *History) Put(id, title string) []HistoryItem {
	if id == "" {
		return h.List()
	}
	h.mu.Lock()
	defer h.mu.Unlock()

	out := make([]HistoryItem, 0, len(h.items)+1)
	out = append(out, HistoryItem{ID: id, Title: title})
	for _, it := range h.items {
		if it.ID != id {
			out = append(out, it)
		}
	}
	if len(out) > historyMax {
		out = out[:historyMax]
	}
	h.items = out
	h.saveLocked()
	return h.listLocked()
}

func (h *History) Delete(id string) []HistoryItem {
	h.mu.Lock()
	defer h.mu.Unlock()
	out := make([]HistoryItem, 0, len(h.items)) // 非 nil，落盘是 []
	for _, it := range h.items {
		if it.ID != id {
			out = append(out, it)
		}
	}
	h.items = out
	h.saveLocked()
	return h.listLocked()
}

func (h *History) Clear() []HistoryItem {
	h.mu.Lock()
	defer h.mu.Unlock()
	// 用空切片而不是 nil：nil 经 json.Marshal 会写成 `null`，
	// 落在磁盘上就是个看不懂的东西（读回来虽然也是空表）。
	// 文件里应该是 `[]`，和人手改的期望一致。
	h.items = []HistoryItem{}
	h.saveLocked()
	// ⚠️ 返回**空切片**而不是 nil：nil 序列化成 JSON 是 `null`，
	// 而客户端的 `HistoryResp { items: Vec<HistoryItem> }` 没有
	// `#[serde(default)]`，收到 null 会解析失败 —— 清空历史之后
	// 客户端反而报「取观看记录失败」。
	return []HistoryItem{}
}

// saveLocked 原子写（先 .part 再 rename）。调用方必须持有锁。
func (h *History) saveLocked() {
	data, err := json.Marshal(h.items)
	if err != nil {
		log.Printf("历史记录序列化失败：%v", err)
		return
	}
	if dir := filepath.Dir(h.path); dir != "." && dir != "" {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			log.Printf("历史记录目录不可用：%v", err)
			return
		}
	}
	// 先写 .part 再改名：Kindle 是 FAT32，直接覆写时断电会留下半截 JSON，
	// 下次启动解析失败就会把用户的记录当空表。
	tmp := h.path + ".part"
	if err := os.WriteFile(tmp, data, 0o644); err != nil {
		log.Printf("写历史记录失败：%v（路径 %s）", err, tmp)
		return
	}
	if err := os.Rename(tmp, h.path); err != nil {
		log.Printf("提交历史记录失败：%v", err)
	}
}

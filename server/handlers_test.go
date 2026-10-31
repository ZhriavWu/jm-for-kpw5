package main

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"testing"
)

// 起一个只装了历史记录的最小 Server（这些接口不碰上游）。
func testServer(t *testing.T) (*Server, *httptest.Server) {
	t.Helper()
	dir := t.TempDir()
	cache, err := NewCache(filepath.Join(dir, "cache"), 0)
	if err != nil {
		t.Fatal(err)
	}
	s := &Server{
		jcReady: make(chan struct{}),
		cache:   cache,
		hist:    NewHistory(filepath.Join(dir, "history.json")),
		photos:  make(map[string]*photoEntry),
		albums:  make(map[string]*albumEntry),
		search:  NewSearchCache(),
	}
	ts := httptest.NewServer(s.routes())
	t.Cleanup(ts.Close)
	return s, ts
}

func do(t *testing.T, ts *httptest.Server, method, path, body string) map[string]any {
	t.Helper()
	req, err := http.NewRequest(method, ts.URL+path, strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	if body != "" {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("%s %s 请求失败：%v", method, path, err)
	}
	defer resp.Body.Close()
	var out map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatalf("%s %s 响应不是 JSON：%v", method, path, err)
	}
	return out
}

func ids(resp map[string]any) []string {
	raw, _ := resp["items"].([]any)
	out := make([]string, 0, len(raw))
	for _, it := range raw {
		if m, ok := it.(map[string]any); ok {
			out = append(out, m["id"].(string))
		}
	}
	return out
}

// 写→读的完整往返。**不允许卡住** —— 走的是真实 router。
func TestHistoryRoutesRoundTrip(t *testing.T) {
	_, ts := testServer(t)

	if got := ids(do(t, ts, "GET", "/api/history", "")); len(got) != 0 {
		t.Fatalf("初始应为空，实际 %v", got)
	}
	do(t, ts, "POST", "/api/history", `{"id":"111","title":"甲"}`)
	do(t, ts, "POST", "/api/history", `{"id":"222","title":"乙"}`)

	got := ids(do(t, ts, "GET", "/api/history", ""))
	if len(got) != 2 || got[0] != "222" || got[1] != "111" {
		t.Fatalf("顺序应为最新在前 [222 111]，实际 %v", got)
	}

	// 重复 id 不新增，且挪到最前
	do(t, ts, "POST", "/api/history", `{"id":"111","title":"甲改"}`)
	if got := ids(do(t, ts, "GET", "/api/history", "")); len(got) != 2 || got[0] != "111" {
		t.Fatalf("重复写入应挪到最前且不新增，实际 %v", got)
	}

	// 删单条
	if got := ids(do(t, ts, "DELETE", "/api/history/222", "")); len(got) != 1 || got[0] != "111" {
		t.Fatalf("删单条后应剩 [111]，实际 %v", got)
	}
}

// ⭐ 回归测试：`DELETE /api/history` 是**清空全部**，不是「删 id 为 history 的」。
//
// 踩过：用 `lastSegment` 判空，而 `TrimPrefix("/api/history", "/api/history/")`
// 前缀不匹配、原样返回，`lastSegment` 于是得到 "history"（非空）——
// 请求返回 200 但一条都没删掉。
func TestHistoryDeleteAllClearsEverything(t *testing.T) {
	_, ts := testServer(t)

	do(t, ts, "POST", "/api/history", `{"id":"111","title":"甲"}`)
	do(t, ts, "POST", "/api/history", `{"id":"222","title":"乙"}`)
	if n := len(ids(do(t, ts, "GET", "/api/history", ""))); n != 2 {
		t.Fatalf("准备阶段应该有两條，实际 %d", n)
	}

	resp := do(t, ts, "DELETE", "/api/history", "")
	if got := ids(resp); len(got) != 0 {
		t.Fatalf("清空全部后响应里应没有条目，实际 %v", got)
	}
	if got := ids(do(t, ts, "GET", "/api/history", "")); len(got) != 0 {
		t.Fatalf("清空全部后 GET 应为空，实际 %v（说明被当成删单条了）", got)
	}
}

// 清空后返回的必须是 `[]` 而不是 `null` —— 客户端 `Vec<HistoryItem>` 没 serde default。
func TestHistoryClearReturnsArrayNotNull(t *testing.T) {
	_, ts := testServer(t)
	do(t, ts, "POST", "/api/history", `{"id":"111","title":"甲"}`)

	req, _ := http.NewRequest("DELETE", ts.URL+"/api/history", nil)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()

	var raw map[string]json.RawMessage
	if err := json.NewDecoder(resp.Body).Decode(&raw); err != nil {
		t.Fatal(err)
	}
	if string(raw["items"]) != "[]" {
		t.Fatalf("items 应为 []，实际 %s", raw["items"])
	}
}

// 写入非法 JSON 要报 400，而不是 panic 或挂住。
func TestHistoryPostBadJSON(t *testing.T) {
	_, ts := testServer(t)
	req, _ := http.NewRequest("POST", ts.URL+"/api/history", strings.NewReader("不是 JSON"))
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != 400 {
		t.Fatalf("应为 400，实际 %d", resp.StatusCode)
	}
}

// /api/info 不能依赖上游 —— 它是客户端的探活接口，
// 上游还没就绪时也必须立刻回（否则服务端「看起来没起来」）。
func TestInfoDoesNotWaitForUpstream(t *testing.T) {
	_, ts := testServer(t) // jcReady 永远不 close
	resp := do(t, ts, "GET", "/api/info", "")
	if resp["ok"] != true {
		t.Fatalf("/api/info 应该直接成功，实际 %v", resp)
	}
	if resp["version"] != version {
		t.Fatalf("版本号不对：%v", resp["version"])
	}
}

package main

import "testing"

// 期望值由 jmcomic 的 JmImageTool.get_num 跑出来（纯算法，不联网）。
// 客户端 src/descramble.rs 用的是同一组向量 —— 两边都对着同一条基准，
// 服务端算的和客户端算的就不可能不一致。
func TestSegNum(t *testing.T) {
	cases := []struct {
		scramble, aid uint64
		name          string
		want          int
	}{
		{220980, 1475681, "00001", 10},
		{220980, 1475681, "00002", 8},
		{220980, 1475681, "00015", 10},
		{220980, 1475681, "00017", 6},
		{220980, 1475681, "00019", 14},
		{220980, 1475681, "00032", 4},
		{220980, 5888, "00001", 0},
		{220980, 230000, "00001", 10},
		{220980, 268850, "00001", 6},
		{220980, 300000, "00001", 16},
		{220980, 421926, "00001", 14},
		{220980, 500000, "abc", 12},
	}
	for _, c := range cases {
		if got := segNum(c.scramble, c.aid, c.name); got != c.want {
			t.Errorf("segNum(%d, %d, %q) = %d, want %d", c.scramble, c.aid, c.name, got, c.want)
		}
	}
}

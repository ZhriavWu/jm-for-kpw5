package main

import "testing"

// 上游的 actors / works 是 `[]any`，里面可能混进 null 或空串。
// 空标签画到界面上就是一个空框，所以要在服务端就滤掉。
func TestAsStringSlice(t *testing.T) {
	cases := []struct {
		name string
		in   any
		want []string
	}{
		{"正常", []any{"森亚露露卡", "绪山真寻"}, []string{"森亚露露卡", "绪山真寻"}},
		{"过滤空串", []any{"甲", "", "乙"}, []string{"甲", "乙"}},
		{"过滤 null", []any{"甲", nil, "乙"}, []string{"甲", "乙"}},
		{"去首尾空白", []any{"  甲  "}, []string{"甲"}},
		{"只有空白", []any{"   "}, []string{}},
		{"nil", nil, []string{}},
		{"不是数组", "甲", []string{}},
		{"空数组", []any{}, []string{}},
		{"数字也收", []any{float64(123)}, []string{"123"}},
	}
	for _, c := range cases {
		got := asStringSlice(c.in)
		if len(got) != len(c.want) {
			t.Errorf("%s: 得到 %v，期望 %v", c.name, got, c.want)
			continue
		}
		for i := range got {
			if got[i] != c.want[i] {
				t.Errorf("%s: 得到 %v，期望 %v", c.name, got, c.want)
				break
			}
		}
	}
}

// 返回的切片必须是**非 nil**，否则 JSON 里是 `null`，
// 而客户端的 `actors: Vec<String>` 虽然能收 null，但前端拿到的就是「没有这个字段」。
func TestAsStringSliceNeverNil(t *testing.T) {
	for _, in := range []any{nil, []any{}, "x"} {
		if got := asStringSlice(in); got == nil {
			t.Fatalf("输入 %v 返回了 nil，会序列化成 null", in)
		}
	}
}

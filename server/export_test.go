package main

import (
	"archive/zip"
	"bytes"
	"image"
	"image/color"
	"io"
	"strings"
	"testing"
	"time"

	"github.com/deepteams/webp"
	"github.com/laoin114514/jmapi"
)

// 造一张「每行一个唯一灰度」的图，用来验证重排是严格可逆的。
func rampRGBA(w, h int) *image.RGBA {
	img := image.NewRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		v := uint8(y % 251) // 251 是质数，避免取模后单调性被掩盖
		for x := 0; x < w; x++ {
			o := y*img.Stride + x*4
			img.Pix[o], img.Pix[o+1], img.Pix[o+2], img.Pix[o+3] = v, v, v, 255
		}
	}
	return img
}

// scrambleRows 是 rearrangeRows 的**逆运算**：把一张正常图切成源站那种乱序。
func scrambleRows(src *image.RGBA, num int) *image.RGBA {
	b := src.Bounds()
	w, h := b.Dx(), b.Dy()
	over := h % num
	piece := h / num
	dst := image.NewRGBA(image.Rect(0, 0, w, h))
	rb := w * 4
	for i := 0; i < num; i++ {
		move := piece
		yFrom := piece * i
		if i == 0 {
			move += over
		} else {
			yFrom += over
		}
		yTo := h - piece*(i+1) - over
		copy(dst.Pix[yTo*rb:(yTo+move)*rb], src.Pix[yFrom*rb:(yFrom+move)*rb])
	}
	return dst
}

// 重排必须能把切乱的图严格还原成原图（逐像素）。
func TestRearrangeRoundTrip(t *testing.T) {
	for _, num := range []int{2, 4, 6, 8, 10, 12, 14} {
		const w, h = 32, 160
		orig := rampRGBA(w, h)
		scrambled := scrambleRows(orig, num)

		got := rearrangeRows(scrambled, num).(*image.RGBA)
		if !bytes.Equal(got.Pix, orig.Pix) {
			t.Fatalf("num=%d：重排后与原始图不一致", num)
		}
	}
}

// 不加密（num<=1）时应当原样返回，不能把好图切坏。
func TestRearrangeNoop(t *testing.T) {
	orig := rampRGBA(16, 64)
	for _, num := range []int{0, 1, -3} {
		got := rearrangeRows(orig, num)
		if got != image.Image(orig) {
			t.Fatalf("num=%d 时不该改动图片", num)
		}
	}
}

// 编码出来的必须是能再次解开的有效 webp，且有损/无损都支持。
func TestEncodeDescrambledValid(t *testing.T) {
	const w, h = 64, 160
	orig := rampRGBA(w, h)

	// 先用 webp 编码成「正常图」，再当成「源站切乱图」喂给 encodeDescrambled（seg=0，不重排）
	var rawBuf bytes.Buffer
	if err := webp.Encode(&rawBuf, orig, &webp.EncoderOptions{Quality: 95, Method: 4}); err != nil {
		t.Fatalf("准备样本失败：%v", err)
	}

	for _, f := range []exportFormat{exportQ85, exportLossless} {
		out, err := encodeDescrambled(rawBuf.Bytes(), 0, f)
		if err != nil {
			t.Fatalf("格式 %s 编码失败：%v", f, err)
		}
		img, err := webp.Decode(bytes.NewReader(out))
		if err != nil {
			t.Fatalf("格式 %s 的产物解不开：%v", f, err)
		}
		if b := img.Bounds(); b.Dx() != w || b.Dy() != h {
			t.Fatalf("格式 %s 产物尺寸不对：%d×%d", f, b.Dx(), b.Dy())
		}
	}
}

// 重排后的图编码、再解码，内容要跟「直接重排后的像素」一致（有损会有误差，用无损验证）。
func TestEncodeDescrambledLosslessMatches(t *testing.T) {
	const w, h = 40, 120
	orig := rampRGBA(w, h)
	scrambled := scrambleRows(orig, 4)

	var rawBuf bytes.Buffer
	if err := webp.Encode(&rawBuf, scrambled, &webp.EncoderOptions{Quality: 95, Method: 4}); err != nil {
		t.Fatalf("准备样本失败：%v", err)
	}

	out, err := encodeDescrambled(rawBuf.Bytes(), 4, exportLossless)
	if err != nil {
		t.Fatalf("无损编码失败：%v", err)
	}
	img, err := webp.Decode(bytes.NewReader(out))
	if err != nil {
		t.Fatalf("解不开：%v", err)
	}
	// 无损 webp 往返应当逐像素一致（源图是有损的，但这里编码的是解码后的像素，无损保存）
	// 只比灰度（源图编码时是有损的，色值可能与 orig 有极微小差，但重排位置必须对）
	for y := 0; y < h; y++ {
		want := uint8(y % 251)
		r, _, _, _ := img.At(0, y).RGBA()
		got := uint8(r >> 8)
		// 允许有损源带来的 ±2 误差，但顺序不能错
		d := int(got) - int(want)
		if d < -2 || d > 2 {
			t.Fatalf("第 %d 行还原错位：期望≈%d，实际 %d", y, want, got)
		}
	}
}

// **正常图片模式必须让 /api/photo 把 seg 报成 0**，否则客户端会再解密一次，
// 图又被切乱 —— 这是这个开关最容易踩的坑。
func TestNormalModeReportsSegZero(t *testing.T) {
	s, ts := testServer(t)

	const aid = "500000" // > 421926，segNum 会给一个非 0 的真实值
	p := &jmapi.PhotoDetail{ID: aid, Name: "测试", ScrambleID: "220980"}
	imgs := []jmapi.ImageDetail{{
		PhotoID: aid, Index: 1, FileName: "00001", Suffix: ".webp",
		DownloadURL: "http://example.invalid/00001.webp",
	}}
	s.photoMu.Lock()
	s.photos[aid] = &photoEntry{at: time.Now(), detail: p, images: imgs}
	s.photoMu.Unlock()

	segOf := func() float64 {
		resp := do(t, ts, "GET", "/api/photo/"+aid, "")
		list, _ := resp["images"].([]any)
		if len(list) != 1 {
			t.Fatalf("images 长度不对：%v", resp)
		}
		return list[0].(map[string]any)["seg"].(float64)
	}

	// 默认：关 → seg 是真实分段数
	if got := segOf(); got == 0 {
		t.Fatalf("关模式时 seg 不该是 0（应为真实分段数）")
	}

	// 开
	if r := do(t, ts, "POST", "/api/mode", `{"normal":true}`); r["normal"] != true {
		t.Fatalf("开启失败：%v", r)
	}
	if got := segOf(); got != 0 {
		t.Fatalf("开模式时 seg 必须是 0（否则客户端会二次解密），实际 %v", got)
	}

	// 关回去
	do(t, ts, "POST", "/api/mode", `{"normal":false}`)
	if got := segOf(); got == 0 {
		t.Fatalf("关回去之后 seg 应恢复成真实值")
	}
}

// /api/mode 的读写往返 + 参数校验。
func TestModeEndpoint(t *testing.T) {
	_, ts := testServer(t)

	if r := do(t, ts, "GET", "/api/mode", ""); r["normal"] != false {
		t.Fatalf("默认应为关，实际 %v", r)
	}
	do(t, ts, "POST", "/api/mode", `{"normal":true}`)
	if r := do(t, ts, "GET", "/api/mode", ""); r["normal"] != true {
		t.Fatalf("开启后应为开，实际 %v", r)
	}
	// 缺字段要报错，不能默默当成 false 或 true
	if r := do(t, ts, "POST", "/api/mode", `{}`); r["ok"] != false {
		t.Fatalf("缺 normal 字段应报错，实际 %v", r)
	}
}

// **导出必须是 zip，且解压后根目录直接是图片**（用户明确要求）。
//
// 这条测试就是那份需求的机器表达：任何一个条目名里出现 `/`，
// 就说明多了一层目录 —— 文件浏览器里打开会先看到一个文件夹，不是图。
func TestZipImagesAtRoot(t *testing.T) {
	var buf bytes.Buffer
	zw := zip.NewWriter(&buf)
	for i := 1; i <= 3; i++ {
		if err := addZipEntry(zw, zipEntryName(i), []byte("fake-image")); err != nil {
			t.Fatalf("写入第 %d 条失败：%v", i, err)
		}
	}
	if err := zw.Close(); err != nil {
		t.Fatalf("关闭 zip 失败：%v", err)
	}

	zr, err := zip.NewReader(bytes.NewReader(buf.Bytes()), int64(buf.Len()))
	if err != nil {
		t.Fatalf("产出的不是合法 zip：%v", err)
	}
	if len(zr.File) != 3 {
		t.Fatalf("应有 3 个条目，实际 %d", len(zr.File))
	}
	for _, f := range zr.File {
		if strings.Contains(f.Name, "/") {
			t.Fatalf("条目 %q 带了目录层级 —— 根目录必须是图片", f.Name)
		}
		if !strings.HasSuffix(f.Name, ".webp") {
			t.Fatalf("条目 %q 不是 webp", f.Name)
		}
		if f.Method != zip.Store {
			t.Fatalf("条目 %q 应不压缩（Store），实际 method=%d", f.Name, f.Method)
		}
		rc, err := f.Open()
		if err != nil {
			t.Fatalf("打不开条目 %q：%v", f.Name, err)
		}
		b, _ := io.ReadAll(rc)
		rc.Close()
		if string(b) != "fake-image" {
			t.Fatalf("条目 %q 内容损坏：%q", f.Name, b)
		}
	}
}

// 同名章节要各自拿到互不冲突的 zip 名。
func TestUniqueName(t *testing.T) {
	used := map[string]bool{}
	got := []string{
		uniqueName("第1话", used),
		uniqueName("第1话", used),
		uniqueName("第1话", used),
	}
	want := []string{"第1话", "第1话 (2)", "第1话 (3)"}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("第 %d 个名字应为 %q，实际 %q", i+1, want[i], got[i])
		}
	}
}

// 目录名清洗：非法字符换全角、空名兜底、超长截断。
func TestSanitizeName(t *testing.T) {
	cases := []struct{ in, want string }{
		{"", "未命名"},
		{"   ", "未命名"},
		{"a/b\\c:d*e?f\"g<h>i|j", "a／b＼c：d＊e？f＂g＜h＞i｜j"},
		{"正常标题", "正常标题"},
		{"结尾有点...", "结尾有点"},
		{"结尾有空格   ", "结尾有空格"},
	}
	for _, c := range cases {
		if got := sanitizeName(c.in); got != c.want {
			t.Fatalf("sanitizeName(%q) = %q，期望 %q", c.in, got, c.want)
		}
	}

	// 超长要截断到 60 个字符
	long := ""
	for i := 0; i < 100; i++ {
		long += "字"
	}
	if n := len([]rune(sanitizeName(long))); n != 60 {
		t.Fatalf("超长标题应截断到 60，实际 %d", n)
	}
}

// 颜色无关的兜底：非 RGBA 输入也要能重排（draw 会处理）。
func TestRearrangeNonRGBA(t *testing.T) {
	const w, h = 16, 64
	nrgba := image.NewNRGBA(image.Rect(0, 0, w, h))
	for y := 0; y < h; y++ {
		v := uint8(y % 251)
		for x := 0; x < w; x++ {
			nrgba.Set(x, y, color.NRGBA{v, v, v, 255})
		}
	}
	// 不 panic 且尺寸正确即可
	out := rearrangeRows(nrgba, 4)
	if b := out.Bounds(); b.Dx() != w || b.Dy() != h {
		t.Fatalf("尺寸不对：%d×%d", b.Dx(), b.Dy())
	}
}

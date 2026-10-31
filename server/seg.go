package main

import (
	"crypto/md5"
	"strconv"
)

// 禁漫图片分割数的两个分界点（和 jmcomic 的 JmMagicConstants 一致）。
const (
	scramble268850 = 268850
	scramble421926 = 421926
)

// 拿不到 scramble_id 时的兜底值，和 jmcomic 的 `SCRAMBLE_220980` 一致。
// 实测源站返回的 scramble_id 全是 220980。
const fallbackScrambleID = "220980"

// segNum 算出一页要被切成几段。
//
// 算法和 jmcomic 的 `JmImageTool.get_num` **逐字对齐**：
//
//	aid < scramble_id          → 0（这本不加密）
//	aid < 268850               → 10
//	x = aid < 421926 ? 10 : 8
//	s = md5("{aid}{filename}")   # filename 不含后缀
//	num = (ord(s 的最后一个字符) % x) * 2 + 2
//
// ⚠️ 最后一步的 `ord` 是**十六进制字符串里那个字符的 ASCII 码**，
//    不是哈希字节本身。'0'..'9' 是 48..57，'a'..'f' 是 97..102。
//    这里不做这个转换的话，算出来的分段数会整体偏小，图就拼错位。
func segNum(scrambleID, aid uint64, filename string) int {
	if aid < scrambleID {
		return 0
	}
	if aid < scramble268850 {
		return 10
	}
	x := 10
	if aid >= scramble421926 {
		x = 8
	}
	sum := md5.Sum([]byte(strconv.FormatUint(aid, 10) + filename))
	nibble := int(sum[15] & 0x0f)
	var last int
	if nibble < 10 {
		last = 48 + nibble // '0'..'9'
	} else {
		last = 87 + nibble // 'a'..'f'（97-10）
	}
	return (last%x)*2 + 2
}

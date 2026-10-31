package jmapi

import (
	"bytes"
	"crypto/md5"
	"encoding/hex"
	"fmt"
	"image"
	"image/draw"
	"image/gif"
	"image/jpeg"
	"image/png"
	"os"
	"path"
	"path/filepath"
	"strconv"
	"strings"

	// [本地补丁] 注册 webp 解码器。
	// Go 标准库只认 jpeg/png/gif，而禁漫现在绝大多数发 webp，
	// 没有这一行 image.Decode 会报 "unknown format"，整章一张都存不下来。
	_ "golang.org/x/image/webp"
)

func segmentationNum(scrambleID, aid int, filename string) int {
	if aid < scrambleID {
		return 0
	}
	if aid < 268850 {
		return 10
	}

	x := 10
	if aid >= 421926 {
		x = 8
	}
	h := md5.Sum([]byte(fmt.Sprintf("%d%s", aid, filename)))
	hx := hex.EncodeToString(h[:])
	last := int(hx[len(hx)-1])
	return (last%x)*2 + 2
}

func segmentationNumByURL(scrambleID int, imgURL string) int {
	clean := imgURL
	if idx := strings.Index(clean, "?"); idx >= 0 {
		clean = clean[:idx]
	}
	file := path.Base(clean)
	filename := strings.TrimSuffix(file, path.Ext(file))
	aid := extractAlbumIDFromURL(clean)
	if aid <= 0 {
		return 0
	}
	return segmentationNum(scrambleID, aid, filename)
}

func extractAlbumIDFromURL(u string) int {
	clean := u
	if idx := strings.Index(clean, "?"); idx >= 0 {
		clean = clean[:idx]
	}
	parts := strings.Split(clean, "/")
	for i := 0; i < len(parts)-1; i++ {
		if parts[i] == "photos" {
			v, _ := strconv.Atoi(parts[i+1])
			return v
		}
	}
	return 0
}

func decodeAndSaveImage(raw []byte, savePath string, segNum int) error {
	if segNum == 0 {
		return saveImageBytes(raw, savePath)
	}

	img, format, err := image.Decode(bytes.NewReader(raw))
	if err != nil {
		return err
	}
	_ = format

	dst := rearrangeImageBySegmentation(img, segNum)
	return saveImage(dst, savePath)
}

func rearrangeImageBySegmentation(src image.Image, num int) image.Image {
	if num <= 0 {
		return src
	}
	b := src.Bounds()
	w, h := b.Dx(), b.Dy()
	over := h % num
	piece := h / num

	dst := image.NewRGBA(image.Rect(0, 0, w, h))
	for i := 0; i < num; i++ {
		move := piece
		ySrc := h - (piece * (i + 1)) - over
		yDst := piece * i
		if i == 0 {
			move += over
		} else {
			yDst += over
		}
		srcRect := image.Rect(0, ySrc, w, ySrc+move)
		dstRect := image.Rect(0, yDst, w, yDst+move)
		draw.Draw(dst, dstRect, src, srcRect.Min, draw.Src)
	}
	return dst
}

func saveImageBytes(raw []byte, savePath string) error {
	// if target suffix differs, decode and re-encode.
	ext := strings.ToLower(filepath.Ext(savePath))
	if ext == ".gif" {
		return os.WriteFile(savePath, raw, 0o644)
	}
	img, _, err := image.Decode(bytes.NewReader(raw))
	if err != nil {
		return os.WriteFile(savePath, raw, 0o644)
	}
	return saveImage(img, savePath)
}

func saveImage(img image.Image, savePath string) error {
	f, err := os.Create(savePath)
	if err != nil {
		return err
	}
	defer f.Close()

	ext := strings.ToLower(filepath.Ext(savePath))
	switch ext {
	case ".png":
		return png.Encode(f, img)
	case ".gif":
		return gif.Encode(f, img, nil)
	case ".jpg", ".jpeg", "":
		return jpeg.Encode(f, img, &jpeg.Options{Quality: 95})
	default:
		return png.Encode(f, img)
	}
}

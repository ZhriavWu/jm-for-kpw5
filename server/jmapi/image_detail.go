package jmapi

import (
	"fmt"
	"path"
	"path/filepath"
	"strings"
)

// ImageDetail 对齐 Python 版 JmImageDetail：描述一张图的下载信息。
type ImageDetail struct {
	PhotoID   string `json:"photo_id"`
	AlbumID   string `json:"album_id,omitempty"`
	ScrambleID string `json:"scramble_id,omitempty"`

	// DownloadURL 完整 URL（含 query 参数 v=...）
	DownloadURL string `json:"download_url"`

	// ImgName 原始文件名（含后缀），例如 "00001.webp"
	ImgName string `json:"img_name"`
	// FileName 不含后缀，例如 "00001"
	FileName string `json:"file_name"`
	// Suffix 含点后缀，例如 ".webp"
	Suffix string `json:"suffix"`

	// Index 从 1 开始
	Index int `json:"index"`
}

func (p *PhotoDetail) CreateImageDetail(index int) (*ImageDetail, error) {
	if p == nil {
		return nil, fmt.Errorf("photo is nil")
	}
	if index < 0 || index >= len(p.PageArr) {
		return nil, fmt.Errorf("image index out of range: %d >= %d", index, len(p.PageArr))
	}
	imgName := strings.TrimSpace(p.PageArr[index])
	if imgName == "" {
		return nil, fmt.Errorf("empty img name at index %d", index)
	}
	u, err := p.ImageURL(imgName)
	if err != nil {
		return nil, err
	}

	suffix := strings.ToLower(filepath.Ext(imgName))
	nameNoExt := strings.TrimSuffix(imgName, path.Ext(imgName))
	if nameNoExt == "" {
		nameNoExt = imgName
	}

	return &ImageDetail{
		PhotoID:    p.ID,
		AlbumID:    p.ResolvedAlbumID(),
		ScrambleID: p.ScrambleID,
		DownloadURL: u,
		ImgName:    imgName,
		FileName:   nameNoExt,
		Suffix:     suffix,
		Index:      index + 1,
	}, nil
}

func (p *PhotoDetail) Images() ([]ImageDetail, error) {
	if p == nil {
		return nil, fmt.Errorf("photo is nil")
	}
	out := make([]ImageDetail, 0, len(p.PageArr))
	for i := range p.PageArr {
		d, err := p.CreateImageDetail(i)
		if err != nil {
			return nil, err
		}
		out = append(out, *d)
	}
	return out, nil
}


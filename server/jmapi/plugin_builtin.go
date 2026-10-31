package jmapi

import (
	"fmt"
	"log"
	"path/filepath"
	"strconv"
	"strings"
)

// -------- 内置插件 1: 日志主题过滤插件（简化版） --------

type TopicFilterPlugin struct {
	PluginAdapter
	allow map[string]bool
}

func (p *TopicFilterPlugin) Key() string { return "topic_filter" }

func (p *TopicFilterPlugin) Configure(kwargs map[string]any) error {
	p.allow = map[string]bool{}
	if kwargs == nil {
		return nil
	}
	if raw, ok := kwargs["allow"]; ok {
		switch t := raw.(type) {
		case []any:
			for _, item := range t {
				p.allow[strings.ToLower(fmt.Sprintf("%v", item))] = true
			}
		case []string:
			for _, s := range t {
				p.allow[strings.ToLower(s)] = true
			}
		case string:
			for _, s := range strings.Split(t, ",") {
				s = strings.TrimSpace(s)
				if s != "" {
					p.allow[strings.ToLower(s)] = true
				}
			}
		}
	}
	return nil
}

func (p *TopicFilterPlugin) should(topic string) bool {
	if len(p.allow) == 0 {
		return true
	}
	return p.allow[strings.ToLower(topic)]
}

func (p *TopicFilterPlugin) BeforeAlbum(ctx PluginContext, album *AlbumDetail) error {
	if p.should("album") {
		log.Printf("[album.before] %s %s", album.ID, album.Name)
	}
	return nil
}
func (p *TopicFilterPlugin) AfterAlbum(ctx PluginContext, album *AlbumDetail) error {
	if p.should("album") {
		log.Printf("[album.after] %s", album.ID)
	}
	return nil
}
func (p *TopicFilterPlugin) BeforePhoto(ctx PluginContext, photo *PhotoDetail) error {
	if p.should("photo") {
		log.Printf("[photo.before] %s %s", photo.ID, photo.Name)
	}
	return nil
}
func (p *TopicFilterPlugin) AfterPhoto(ctx PluginContext, photo *PhotoDetail) error {
	if p.should("photo") {
		log.Printf("[photo.after] %s", photo.ID)
	}
	return nil
}
func (p *TopicFilterPlugin) BeforeImage(ctx PluginContext, photo *PhotoDetail, imageURL, savePath string) error {
	if p.should("image") {
		log.Printf("[image.before] %s -> %s", imageURL, savePath)
	}
	return nil
}
func (p *TopicFilterPlugin) AfterImage(ctx PluginContext, photo *PhotoDetail, imageURL, savePath string) error {
	if p.should("image") {
		log.Printf("[image.after] %s", savePath)
	}
	return nil
}

// -------- 内置插件 2: 下载后缀过滤插件（简化版） --------

type ImageSuffixFilterPlugin struct {
	PluginAdapter
	suffixSet map[string]bool
}

func (p *ImageSuffixFilterPlugin) Key() string { return "image_suffix_filter" }

func (p *ImageSuffixFilterPlugin) Configure(kwargs map[string]any) error {
	p.suffixSet = map[string]bool{}
	if kwargs == nil {
		return nil
	}

	raw, ok := kwargs["suffixes"]
	if !ok {
		return nil
	}

	add := func(v string) {
		v = strings.TrimSpace(strings.ToLower(v))
		if v == "" {
			return
		}
		if !strings.HasPrefix(v, ".") {
			v = "." + v
		}
		p.suffixSet[v] = true
	}

	switch t := raw.(type) {
	case string:
		for _, s := range strings.Split(t, ",") {
			add(s)
		}
	case []any:
		for _, s := range t {
			add(fmt.Sprintf("%v", s))
		}
	case []string:
		for _, s := range t {
			add(s)
		}
	default:
		return fmt.Errorf("suffixes type not supported: %T", raw)
	}

	return nil
}

func (p *ImageSuffixFilterPlugin) BeforeImage(ctx PluginContext, photo *PhotoDetail, imageURL, savePath string) error {
	if len(p.suffixSet) == 0 {
		return nil
	}
	ext := strings.ToLower(filepath.Ext(savePath))
	if ext == "" {
		return nil
	}
	if !p.suffixSet[ext] {
		return fmt.Errorf("suffix %s blocked by image_suffix_filter", ext)
	}
	return nil
}

// -------- 内置插件 3: 重试增强插件（简化版） --------

type RetryTuningPlugin struct {
	PluginAdapter
	retry int
}

func (p *RetryTuningPlugin) Key() string { return "retry_tuning" }

func (p *RetryTuningPlugin) Configure(kwargs map[string]any) error {
	p.retry = 5
	if kwargs == nil {
		return nil
	}
	if raw, ok := kwargs["retry_times"]; ok {
		switch t := raw.(type) {
		case int:
			p.retry = t
		case int64:
			p.retry = int(t)
		case float64:
			p.retry = int(t)
		case string:
			v, err := strconv.Atoi(strings.TrimSpace(t))
			if err == nil {
				p.retry = v
			}
		}
	}
	if p.retry <= 0 {
		p.retry = 1
	}
	return nil
}

func (p *RetryTuningPlugin) AfterInit(ctx PluginContext) error {
	if ctx.Option == nil {
		return nil
	}
	ctx.Option.ClientConfig.RetryTimes = p.retry
	if ctx.Client != nil {
		ctx.Client.cfg.RetryTimes = p.retry
	}
	return nil
}

func init() {
	RegisterPluginFactory("topic_filter", func() Plugin { return &TopicFilterPlugin{} })
	RegisterPluginFactory("image_suffix_filter", func() Plugin { return &ImageSuffixFilterPlugin{} })
	RegisterPluginFactory("retry_tuning", func() Plugin { return &RetryTuningPlugin{} })
}

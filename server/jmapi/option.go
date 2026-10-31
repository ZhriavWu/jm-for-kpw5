package jmapi

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

type Option struct {
	Log          bool
	Version      string
	ClientConfig Config
	DirRule      DirRule
	Download     DownloadOptions
	Plugins      PluginGroup
	UseCache     bool
}

type DownloadOptions struct {
	Image     ImageOptions
	Threading ThreadingOptions
}

type ImageOptions struct {
	Decode bool
	Suffix string
}

type ThreadingOptions struct {
	Image int
	Photo int
}

type DirRule struct {
	Rule        string
	BaseDir     string
	NormalizeZH string
}

type PluginGroup struct {
	Valid       string
	BeforeAlbum []PluginConfig
	AfterAlbum  []PluginConfig
	BeforePhoto []PluginConfig
	AfterPhoto  []PluginConfig
	BeforeImage []PluginConfig
	AfterImage  []PluginConfig
	AfterInit   []PluginConfig
}

type PluginConfig struct {
	Plugin string
	Kwargs map[string]any
	Safe   bool
	Log    bool
	Valid  string
}

func DefaultOption() Option {
	wd, _ := os.Getwd()
	photoWorkers := runtime.NumCPU()
	return Option{
		Log:     true,
		Version: "2.1",
		ClientConfig: Config{
			ClientType:        ClientTypeAPI,
			RetryTimes:        5,
			AutoUpdateHost:    true,
			AutoEnsureCookies: true,
			UseFixedTimestamp: true,
		},
		DirRule: DirRule{
			Rule:    "Bd_Pname",
			BaseDir: wd,
		},
		Download: DownloadOptions{
			Image: ImageOptions{
				Decode: true,
				Suffix: "",
			},
			Threading: ThreadingOptions{
				Image: 30,
				Photo: photoWorkers,
			},
		},
		Plugins: PluginGroup{
			Valid: "log",
		},
		UseCache: true,
	}
}

func LoadOption(path string) (Option, error) {
	opt := DefaultOption()
	if path == "" {
		return opt, nil
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return opt, err
	}
	if err := mergeOptionFromYAML(&opt, data); err != nil {
		return opt, err
	}
	return opt, nil
}

func LoadOptionFromYAMLText(text string) (Option, error) {
	opt := DefaultOption()
	if strings.TrimSpace(text) == "" {
		return opt, nil
	}
	if err := mergeOptionFromYAML(&opt, []byte(text)); err != nil {
		return opt, err
	}
	return opt, nil
}

func mergeOptionFromYAML(opt *Option, data []byte) error {
	var raw map[string]any
	if err := yaml.Unmarshal(data, &raw); err != nil {
		return err
	}

	if v, ok := raw["log"]; ok {
		opt.Log = toBool(v, opt.Log)
	}
	if v, ok := raw["version"]; ok {
		opt.Version = toString(v, opt.Version)
	}

	if v, ok := asMap(raw["dir_rule"]); ok {
		opt.DirRule.Rule = toString(v["rule"], opt.DirRule.Rule)
		opt.DirRule.BaseDir = toString(v["base_dir"], opt.DirRule.BaseDir)
		opt.DirRule.NormalizeZH = toString(v["normalize_zh"], opt.DirRule.NormalizeZH)
	}

	if v, ok := asMap(raw["download"]); ok {
		if cache, ok := v["cache"]; ok {
			opt.UseCache = toBool(cache, opt.UseCache)
		}
		if img, ok := asMap(v["image"]); ok {
			opt.Download.Image.Decode = toBool(img["decode"], opt.Download.Image.Decode)
			opt.Download.Image.Suffix = toString(img["suffix"], opt.Download.Image.Suffix)
		}
		if th, ok := asMap(v["threading"]); ok {
			opt.Download.Threading.Image = toIntWithDefault(th["image"], opt.Download.Threading.Image)
			opt.Download.Threading.Photo = toIntWithDefault(th["photo"], opt.Download.Threading.Photo)
		}
	}

	if v, ok := asMap(raw["client"]); ok {
		opt.ClientConfig.ClientType = ClientType(toString(v["impl"], string(opt.ClientConfig.ClientType)))
		opt.ClientConfig.RetryTimes = toIntWithDefault(v["retry_times"], opt.ClientConfig.RetryTimes)

		if domain, ok := asSlice(v["domain"]); ok {
			domains := make([]string, 0, len(domain))
			for _, item := range domain {
				d := strings.TrimSpace(toString(item, ""))
				if d != "" {
					domains = append(domains, d)
				}
			}
			if len(domains) > 0 {
				opt.ClientConfig.Domains = domains
			}
		}

		if postman, ok := asMap(v["postman"]); ok {
			if metaData, ok := asMap(postman["meta_data"]); ok {
				if headers, ok := asMap(metaData["headers"]); ok {
					if opt.ClientConfig.Headers == nil {
						opt.ClientConfig.Headers = map[string]string{}
					}
					for k, hv := range headers {
						opt.ClientConfig.Headers[k] = toString(hv, "")
					}
				}
				if cookies, ok := asMap(metaData["cookies"]); ok {
					if opt.ClientConfig.Cookies == nil {
						opt.ClientConfig.Cookies = map[string]string{}
					}
					for k, cv := range cookies {
						opt.ClientConfig.Cookies[k] = toString(cv, "")
					}
				}
			}
		}
	}

	if v, ok := asMap(raw["plugins"]); ok {
		opt.Plugins.Valid = toString(v["valid"], opt.Plugins.Valid)
		opt.Plugins.AfterInit = parsePluginList(v["after_init"])
		opt.Plugins.BeforeAlbum = parsePluginList(v["before_album"])
		opt.Plugins.AfterAlbum = parsePluginList(v["after_album"])
		opt.Plugins.BeforePhoto = parsePluginList(v["before_photo"])
		opt.Plugins.AfterPhoto = parsePluginList(v["after_photo"])
		opt.Plugins.BeforeImage = parsePluginList(v["before_image"])
		opt.Plugins.AfterImage = parsePluginList(v["after_image"])
	}

	if strings.TrimSpace(opt.DirRule.BaseDir) == "" {
		wd, _ := os.Getwd()
		opt.DirRule.BaseDir = wd
	}
	return nil
}

func parsePluginList(v any) []PluginConfig {
	items, ok := asSlice(v)
	if !ok || len(items) == 0 {
		return nil
	}
	ret := make([]PluginConfig, 0, len(items))
	for _, item := range items {
		m, ok := asMap(item)
		if !ok {
			continue
		}
		pc := PluginConfig{
			Plugin: toString(m["plugin"], ""),
			Safe:   toBool(m["safe"], true),
			Log:    toBool(m["log"], true),
			Valid:  toString(m["valid"], ""),
		}
		if kw, ok := asMap(m["kwargs"]); ok {
			pc.Kwargs = kw
		}
		if strings.TrimSpace(pc.Plugin) != "" {
			ret = append(ret, pc)
		}
	}
	return ret
}

func (o Option) NewClient() *Client {
	return NewClient(o.ClientConfig)
}

func (o Option) DecideImageSuffix(original string) string {
	if s := strings.TrimSpace(o.Download.Image.Suffix); s != "" {
		if !strings.HasPrefix(s, ".") {
			return "." + s
		}
		return s
	}
	return original
}

func (o Option) DecideImageFilename(index int) string {
	return fmt.Sprintf("%05d", index)
}

func (o Option) DecideImageSaveDir(album AlbumDetail, photo PhotoDetail) (string, error) {
	rule := strings.TrimSpace(o.DirRule.Rule)
	if rule == "" {
		rule = "Bd_Pname"
	}
	base := strings.TrimSpace(o.DirRule.BaseDir)
	if base == "" {
		base, _ = os.Getwd()
	}

	parts := splitRule(rule)
	paths := make([]string, 0, len(parts))
	for _, r := range parts {
		paths = append(paths, o.parseRuleToken(r, album, photo, base))
	}

	out := filepath.Join(paths...)
	out = filepath.Clean(out)
	return out, nil
}

func (o Option) parseRuleToken(token string, album AlbumDetail, photo PhotoDetail, base string) string {
	token = strings.TrimSpace(token)
	switch token {
	case "Bd":
		return base
	case "Pname":
		return sanitizePath(photo.Name)
	case "Pid":
		return sanitizePath(photo.ID)
	case "Aname":
		return sanitizePath(album.Name)
	case "Aid":
		return sanitizePath(album.ID)
	default:
		if strings.Contains(token, "{") && strings.Contains(token, "}") {
			repl := map[string]string{
				"Aname": album.Name,
				"Aid":   album.ID,
				"Pname": photo.Name,
				"Pid":   photo.ID,
			}
			for k, v := range repl {
				token = strings.ReplaceAll(token, "{"+k+"}", sanitizePath(v))
			}
		}
		return sanitizePath(token)
	}
}

func splitRule(rule string) []string {
	if rule == "" {
		return []string{"Bd"}
	}
	var parts []string
	if strings.Contains(rule, "/") {
		parts = strings.Split(rule, "/")
	} else if strings.Contains(rule, "_") {
		parts = strings.Split(rule, "_")
	} else {
		parts = []string{rule}
	}
	if len(parts) == 0 || strings.TrimSpace(parts[0]) != "Bd" {
		parts = append([]string{"Bd"}, parts...)
	}
	for i := range parts {
		parts[i] = strings.TrimSpace(parts[i])
	}
	return parts
}

func sanitizePath(name string) string {
	name = strings.TrimSpace(name)
	replacer := strings.NewReplacer(
		"\\", "_",
		"/", "_",
		":", "_",
		"*", "_",
		"?", "_",
		"\"", "_",
		"<", "_",
		">", "_",
		"|", "_",
	)
	name = replacer.Replace(name)
	if runtime.GOOS == "windows" {
		name = strings.TrimRight(name, ". ")
	}
	if name == "" {
		return "unknown"
	}
	return name
}

func asMap(v any) (map[string]any, bool) {
	if v == nil {
		return nil, false
	}
	m, ok := v.(map[string]any)
	return m, ok
}

func asSlice(v any) ([]any, bool) {
	if v == nil {
		return nil, false
	}
	s, ok := v.([]any)
	return s, ok
}

func toString(v any, def string) string {
	if v == nil {
		return def
	}
	s := strings.TrimSpace(fmt.Sprintf("%v", v))
	if s == "" || s == "<nil>" {
		return def
	}
	return s
}

func toBool(v any, def bool) bool {
	if v == nil {
		return def
	}
	switch t := v.(type) {
	case bool:
		return t
	case string:
		if t == "" {
			return def
		}
		b, err := strconv.ParseBool(strings.TrimSpace(t))
		if err != nil {
			return def
		}
		return b
	default:
		return def
	}
}

func toIntWithDefault(v any, def int) int {
	if v == nil {
		return def
	}
	switch t := v.(type) {
	case int:
		return t
	case int64:
		return int(t)
	case float64:
		return int(t)
	case string:
		t = strings.TrimSpace(t)
		if t == "" {
			return def
		}
		i, err := strconv.Atoi(t)
		if err != nil {
			return def
		}
		return i
	default:
		return def
	}
}

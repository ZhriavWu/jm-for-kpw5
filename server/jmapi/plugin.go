package jmapi

import (
	"fmt"
	"log"
	"strings"
	"sync"
)

type PluginContext struct {
	Option     *Option
	Downloader *Downloader
	Client     *Client
}

type Plugin interface {
	Key() string
	Configure(kwargs map[string]any) error
	AfterInit(ctx PluginContext) error
	BeforeAlbum(ctx PluginContext, album *AlbumDetail) error
	AfterAlbum(ctx PluginContext, album *AlbumDetail) error
	BeforePhoto(ctx PluginContext, photo *PhotoDetail) error
	AfterPhoto(ctx PluginContext, photo *PhotoDetail) error
	BeforeImage(ctx PluginContext, photo *PhotoDetail, imageURL string, savePath string) error
	AfterImage(ctx PluginContext, photo *PhotoDetail, imageURL string, savePath string) error
}

type PluginAdapter struct{}

func (PluginAdapter) Key() string { return "adapter" }
func (PluginAdapter) Configure(kwargs map[string]any) error { return nil }
func (PluginAdapter) AfterInit(ctx PluginContext) error { return nil }
func (PluginAdapter) BeforeAlbum(ctx PluginContext, album *AlbumDetail) error { return nil }
func (PluginAdapter) AfterAlbum(ctx PluginContext, album *AlbumDetail) error { return nil }
func (PluginAdapter) BeforePhoto(ctx PluginContext, photo *PhotoDetail) error { return nil }
func (PluginAdapter) AfterPhoto(ctx PluginContext, photo *PhotoDetail) error { return nil }
func (PluginAdapter) BeforeImage(ctx PluginContext, photo *PhotoDetail, imageURL string, savePath string) error {
	return nil
}
func (PluginAdapter) AfterImage(ctx PluginContext, photo *PhotoDetail, imageURL string, savePath string) error {
	return nil
}

type pluginEntry struct {
	plugin Plugin
	safe   bool
	log    bool
	valid  string
}

type PluginManager struct {
	mu      sync.RWMutex
	entries []pluginEntry
}

func NewPluginManager() *PluginManager {
	return &PluginManager{}
}

func (pm *PluginManager) Register(p Plugin) {
	if p == nil {
		return
	}
	pm.RegisterWithPolicy(p, true, true, "log")
}

func (pm *PluginManager) RegisterWithPolicy(p Plugin, safe bool, logEnable bool, valid string) {
	if p == nil {
		return
	}
	if valid == "" {
		valid = "log"
	}
	pm.mu.Lock()
	defer pm.mu.Unlock()
	pm.entries = append(pm.entries, pluginEntry{plugin: p, safe: safe, log: logEnable, valid: strings.ToLower(valid)})
}

func (pm *PluginManager) run(fn func(Plugin) error) error {
	pm.mu.RLock()
	entries := append([]pluginEntry{}, pm.entries...)
	pm.mu.RUnlock()

	for _, e := range entries {
		err := fn(e.plugin)
		if err == nil {
			continue
		}
		if !e.safe {
			return err
		}

		switch e.valid {
		case "ignore":
			continue
		case "raise":
			return err
		default: // log
			if e.log {
				log.Printf("plugin [%s] error: %v", e.plugin.Key(), err)
			}
		}
	}
	return nil
}

func (pm *PluginManager) AfterInit(ctx PluginContext) error {
	return pm.run(func(p Plugin) error { return p.AfterInit(ctx) })
}
func (pm *PluginManager) BeforeAlbum(ctx PluginContext, album *AlbumDetail) error {
	return pm.run(func(p Plugin) error { return p.BeforeAlbum(ctx, album) })
}
func (pm *PluginManager) AfterAlbum(ctx PluginContext, album *AlbumDetail) error {
	return pm.run(func(p Plugin) error { return p.AfterAlbum(ctx, album) })
}
func (pm *PluginManager) BeforePhoto(ctx PluginContext, photo *PhotoDetail) error {
	return pm.run(func(p Plugin) error { return p.BeforePhoto(ctx, photo) })
}
func (pm *PluginManager) AfterPhoto(ctx PluginContext, photo *PhotoDetail) error {
	return pm.run(func(p Plugin) error { return p.AfterPhoto(ctx, photo) })
}
func (pm *PluginManager) BeforeImage(ctx PluginContext, photo *PhotoDetail, imageURL, savePath string) error {
	return pm.run(func(p Plugin) error { return p.BeforeImage(ctx, photo, imageURL, savePath) })
}
func (pm *PluginManager) AfterImage(ctx PluginContext, photo *PhotoDetail, imageURL, savePath string) error {
	return pm.run(func(p Plugin) error { return p.AfterImage(ctx, photo, imageURL, savePath) })
}

type PluginFactory func() Plugin

var pluginRegistry = map[string]PluginFactory{}

func RegisterPluginFactory(key string, factory PluginFactory) {
	if key == "" || factory == nil {
		return
	}
	pluginRegistry[strings.ToLower(key)] = factory
}

func BuildPluginFromConfig(cfg PluginConfig) (Plugin, error) {
	f := pluginRegistry[strings.ToLower(cfg.Plugin)]
	if f == nil {
		return nil, fmt.Errorf("unregistered plugin: %s", cfg.Plugin)
	}
	p := f()
	if err := p.Configure(cfg.Kwargs); err != nil {
		return nil, err
	}
	return p, nil
}

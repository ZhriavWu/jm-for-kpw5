package jmapi

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"sync"
)

type Downloader struct {
	Option   Option
	Client   *Client
	Plugins  *PluginManager

	mu              sync.Mutex
	SuccessImages   map[string][]string // photoID -> saved paths
	FailedImages    []ImageFailure
	FailedPhotos    []PhotoFailure
}

type ImageFailure struct {
	PhotoID   string
	ImageURL  string
	SavePath  string
	Err       error
}

type PhotoFailure struct {
	PhotoID string
	Err     error
}

func NewDownloader(option Option) *Downloader {
	client := option.NewClient()
	d := &Downloader{
		Option:        option,
		Client:        client,
		Plugins:       NewPluginManager(),
		SuccessImages: map[string][]string{},
	}

	_ = d.registerConfiguredPlugins()
	_ = d.Plugins.AfterInit(PluginContext{Option: &d.Option, Downloader: d, Client: d.Client})
	return d
}

func (d *Downloader) RegisterPlugin(plugin Plugin) {
	d.Plugins.Register(plugin)
}

func (d *Downloader) registerConfiguredPlugins() error {
	groups := [][]PluginConfig{
		d.Option.Plugins.AfterInit,
		d.Option.Plugins.BeforeAlbum,
		d.Option.Plugins.AfterAlbum,
		d.Option.Plugins.BeforePhoto,
		d.Option.Plugins.AfterPhoto,
		d.Option.Plugins.BeforeImage,
		d.Option.Plugins.AfterImage,
	}

	seen := map[string]bool{}
	for _, g := range groups {
		for _, cfg := range g {
			key := cfg.Plugin
			if key == "" || seen[key] {
				continue
			}
			seen[key] = true

			p, err := BuildPluginFromConfig(cfg)
			if err != nil {
				if cfg.Safe {
					continue
				}
				return err
			}
			d.Plugins.RegisterWithPolicy(p, cfg.Safe, cfg.Log, cfg.Valid)
		}
	}
	return nil
}

func (d *Downloader) DownloadAlbum(albumID string) (*AlbumDetail, error) {
	album, err := d.Client.GetAlbumDetail(albumID)
	if err != nil {
		return nil, err
	}

	ctx := PluginContext{Option: &d.Option, Downloader: d, Client: d.Client}
	_ = d.Plugins.BeforeAlbum(ctx, album)
	defer func() {
		_ = d.Plugins.AfterAlbum(ctx, album)
	}()

	photoIDs := album.EpisodeIDs
	if len(photoIDs) == 0 {
		photoIDs = []string{album.ID}
	}

	workerCount := d.Option.Download.Threading.Photo
	if workerCount <= 0 {
		workerCount = 1
	}
	jobs := make(chan string)
	var wg sync.WaitGroup

	for i := 0; i < workerCount; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for pid := range jobs {
				if _, err := d.DownloadPhoto(pid); err != nil {
					d.mu.Lock()
					d.FailedPhotos = append(d.FailedPhotos, PhotoFailure{PhotoID: pid, Err: err})
					d.mu.Unlock()
				}
			}
		}()
	}

	for _, pid := range photoIDs {
		jobs <- pid
	}
	close(jobs)
	wg.Wait()

	return album, nil
}

func (d *Downloader) DownloadPhoto(photoID string) (*PhotoDetail, error) {
	photo, err := d.Client.GetPhotoDetail(photoID, true, true)
	if err != nil {
		return nil, err
	}

	ctx := PluginContext{Option: &d.Option, Downloader: d, Client: d.Client}
	_ = d.Plugins.BeforePhoto(ctx, photo)
	defer func() {
		_ = d.Plugins.AfterPhoto(ctx, photo)
	}()

	album := AlbumDetail{ID: photo.AlbumID, Name: photo.AlbumID}
	saveDir, err := d.Option.DecideImageSaveDir(album, *photo)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(saveDir, 0o755); err != nil {
		return nil, err
	}

	imageWorkerCount := d.Option.Download.Threading.Image
	if imageWorkerCount <= 0 {
		imageWorkerCount = 1
	}

	type imageJob struct {
		index int
		name  string
		url   string
		suffix string
	}

	jobs := make(chan imageJob)
	var wg sync.WaitGroup

	for i := 0; i < imageWorkerCount; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for job := range jobs {
				if job.url == "" {
					continue
				}
				suffix := job.suffix
				if suffix == "" {
					suffix = ".jpg"
				}
				suffix = d.Option.DecideImageSuffix(suffix)
				savePath := filepath.Join(saveDir, d.Option.DecideImageFilename(job.index)+suffix)

				_ = d.Plugins.BeforeImage(ctx, photo, job.url, savePath)
				if err := d.downloadOneImage(photo, job.url, savePath); err != nil {
					d.mu.Lock()
					d.FailedImages = append(d.FailedImages, ImageFailure{
						PhotoID:  photo.ID,
						ImageURL: job.url,
						SavePath: savePath,
						Err:      err,
					})
					d.mu.Unlock()
					continue
				}
				_ = d.Plugins.AfterImage(ctx, photo, job.url, savePath)
			}
		}()
	}

	images, err := photo.Images()
	if err != nil {
		return nil, err
	}
	for _, img := range images {
		jobs <- imageJob{index: img.Index, name: img.ImgName, url: img.DownloadURL, suffix: img.Suffix}
	}
	close(jobs)
	wg.Wait()

	return photo, nil
}

func (d *Downloader) downloadOneImage(photo *PhotoDetail, imageURL, savePath string) error {
	data, err := d.Client.DownloadImage(imageURL)
	if err != nil {
		return err
	}

	segNum := 0
	if d.Option.Download.Image.Decode {
		sid, _ := strconv.Atoi(photo.ScrambleID)
		segNum = segmentationNumByURL(sid, imageURL)
		// [调试] 看实际用的分段数和 URL
		fmt.Printf("      [debug] scramble=%q sid=%d url=%s segNum=%d\n",
			photo.ScrambleID, sid, imageURL, segNum)
	}
	if err := decodeAndSaveImage(data, savePath, segNum); err != nil {
		return err
	}

	d.mu.Lock()
	d.SuccessImages[photo.ID] = append(d.SuccessImages[photo.ID], savePath)
	d.mu.Unlock()
	return nil
}

func (d *Downloader) HasFailures() bool {
	return len(d.FailedPhotos) > 0 || len(d.FailedImages) > 0
}

func (d *Downloader) RaiseIfHasFailures() error {
	if !d.HasFailures() {
		return nil
	}
	return fmt.Errorf("partial download failed: photo_failures=%d image_failures=%d", len(d.FailedPhotos), len(d.FailedImages))
}

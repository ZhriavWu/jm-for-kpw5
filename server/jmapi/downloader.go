package jmapi

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"sync"
)

type Downloader struct {
	Option  Option
	Client  *Client
	Plugins *PluginManager

	// [本地补丁] 进度与取消钩子。两个都可以是 nil。
	//
	// 上游没有这两个口子，导出时就没法显示进度、也没法「取消」——
	// 而 Kindle 上一章 90 页要跑一两分钟，用户必须有办法停下来。
	//
	//   OnImage：每成功保存一张图调用一次（在下载 goroutine 里，要自己加锁）
	//   Cancel ：返回 true 时停止后续下载；**已经下完的保留**
	OnImage func()
	Cancel  func() bool

	// [本地补丁] ReleaseMemory 每处理完一张图调用一次。
	//
	// 为什么要这个：Kindle 上真机 OOM 过 —— 导出 3 个 worker 同时解码
	// 全分辨率图（每张 ~9MB RGBA，加上重排缓冲和编码器工作区约 30~40MB），
	// 峰值把系统内存吃到只剩 12MB，内核 OOM killer 杀了 Kindle 框架进程
	// （cvm / mesquite），设备上表现为「框架异常重启」。
	//
	// 服务端会把它设成 `debug.FreeOSMemory`，让 Go 尽早把内存还给系统。
	ReleaseMemory func()

	// [本地补丁] 解码锁：同一时刻只允许一个 worker 做「解码+重排+编码」。
	//
	// 下载可以并发（网络才是瓶颈），但解码是内存大户 ——
	// 串起来之后峰值从「N 张图」降到「1 张图」。
	decodeMu sync.Mutex

	// [本地补丁] SegOf 覆盖「这一页切几段」的算法。
	//
	// 上游默认用 `segmentationNumByURL`：从图片 URL 里扒出 `/photos/<aid>/`
	// 和文件名再算。这依赖 URL 的形状 —— 一旦源站换了路径结构，就会算成 0
	// （不解密），图就是花的，而且**不报错**。
	//
	// 调用方手上已经有章节 ID 和原始文件名，按 jmcomic 的原始公式算更可靠。
	// 返回 <= 0 时退回上游的默认算法。
	//
	// 参数 imgName 是带后缀的原始文件名（如 `00001.webp`）。
	SegOf func(photo *PhotoDetail, imgName string) int

	mu            sync.Mutex
	SuccessImages map[string][]string // photoID -> saved paths
	FailedImages  []ImageFailure
	FailedPhotos  []PhotoFailure
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
	return NewDownloaderWithClient(option, option.NewClient())
}

// [本地补丁] 用**已有的** Client 造 Downloader。
//
// 为什么需要：`NewDownloader` 会再 `option.NewClient()`，也就是第二个客户端
// —— 它会重新更新域名列表、重新建连接池。内置服务端已经有一个初始化好的
// `s.jc`（还带 1.7 秒的域名探测），复用它是唯一合理的选择。
func NewDownloaderWithClient(option Option, client *Client) *Downloader {
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

// [本地补丁] 已经成功保存了多少张图（累加所有章节）。
func (d *Downloader) SuccessCount() int {
	d.mu.Lock()
	defer d.mu.Unlock()
	n := 0
	for _, paths := range d.SuccessImages {
		n += len(paths)
	}
	return n
}

// [本地补丁] 调用方传入的取消判断。没有钩子时永远「不取消」。
func (d *Downloader) cancelled() bool {
	return d.Cancel != nil && d.Cancel()
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
		// [本地补丁] 取消之后不再派发新章节
		if d.cancelled() {
			break
		}
		jobs <- pid
	}
	close(jobs)
	wg.Wait()

	return album, nil
}

func (d *Downloader) DownloadPhoto(photoID string) (*PhotoDetail, error) {
	// [本地补丁] 两个 false 见 DownloadOptions 的注释：true 会多两次慢请求。
	photo, err := d.Client.GetPhotoDetail(
		photoID,
		d.Option.Download.FetchAlbum,
		d.Option.Download.FetchScrambleID,
	)
	if err != nil {
		return nil, err
	}
	images, err := photo.Images()
	if err != nil {
		return nil, err
	}
	return photo, d.DownloadPhotoWith(photo, images)
}

// [本地补丁] 用**已经拿到的**章节详情和图片列表下载一话。
//
// 调用方（内置服务端）本来就有这两样东西了 —— 再让 DownloadPhoto
// 去请求一次 `/api/photo` 纯属浪费，而且那是真机上一次 3~20 秒的往返。
func (d *Downloader) DownloadPhotoWith(photo *PhotoDetail, images []ImageDetail) error {
	if photo == nil {
		return fmt.Errorf("photo is nil")
	}

	ctx := PluginContext{Option: &d.Option, Downloader: d, Client: d.Client}
	_ = d.Plugins.BeforePhoto(ctx, photo)
	defer func() {
		_ = d.Plugins.AfterPhoto(ctx, photo)
	}()

	album := AlbumDetail{ID: photo.AlbumID, Name: photo.AlbumID}
	saveDir, err := d.Option.DecideImageSaveDir(album, *photo)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(saveDir, 0o755); err != nil {
		return err
	}

	imageWorkerCount := d.Option.Download.Threading.Image
	if imageWorkerCount <= 0 {
		imageWorkerCount = 1
	}

	type imageJob struct {
		index  int
		name   string
		url    string
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
				// [本地补丁] 取消检查放在每个任务开头：正在下的那几张会下完，
				// 后面的不再发起。
				if d.cancelled() {
					continue
				}
				suffix := job.suffix
				if suffix == "" {
					suffix = ".jpg"
				}
				suffix = d.Option.DecideImageSuffix(suffix)
				savePath := filepath.Join(saveDir, d.Option.DecideImageFilename(job.index)+suffix)

				_ = d.Plugins.BeforeImage(ctx, photo, job.url, savePath)
				if err := d.downloadOneImage(photo, job.name, job.url, savePath); err != nil {
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
				// [本地补丁] 进度回调
				if d.OnImage != nil {
					d.OnImage()
				}
			}
		}()
	}

	for _, img := range images {
		// 发任务前也看一眼：取消之后就别再往通道里塞了
		if d.cancelled() {
			break
		}
		jobs <- imageJob{index: img.Index, name: img.ImgName, url: img.DownloadURL, suffix: img.Suffix}
	}
	close(jobs)
	wg.Wait()

	return nil
}

func (d *Downloader) downloadOneImage(photo *PhotoDetail, imgName, imageURL, savePath string) error {
	data, err := d.Client.DownloadImage(imageURL)
	if err != nil {
		return err
	}

	segNum := 0
	if d.Option.Download.Image.Decode {
		// [本地补丁] 有 SegOf 就用它；返回 <= 0 时退回上游按 URL 猜的算法。
		if d.SegOf != nil {
			segNum = d.SegOf(photo, imgName)
		}
		if segNum <= 0 {
			sid, _ := strconv.Atoi(photo.ScrambleID)
			segNum = segmentationNumByURL(sid, imageURL)
		}
	}

	// [本地补丁] 解码这一段串行化 —— 内存峰值从「N 张全分辨率图」降到 1 张。
	d.decodeMu.Lock()
	err = decodeAndSaveImage(data, savePath, segNum, d.Option.Download.Image)
	d.decodeMu.Unlock()

	// 不管成败都把这一张的缓冲还给系统（失败时可能已经占了一半）
	if d.ReleaseMemory != nil {
		d.ReleaseMemory()
	}
	if err != nil {
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

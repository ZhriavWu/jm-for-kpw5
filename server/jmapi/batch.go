package jmapi

import (
	"fmt"
	"sync"
)

type AlbumBatchResult struct {
	ID    string
	Album *AlbumDetail
	Err   error
}

type PhotoBatchResult struct {
	ID    string
	Photo *PhotoDetail
	Err   error
}

// DownloadAlbumsBatch 并发下载多个 album（走 Downloader 框架）。
func DownloadAlbumsBatch(option Option, albumIDs []string, workers int) []AlbumBatchResult {
	if workers <= 0 {
		workers = 1
	}
	results := make([]AlbumBatchResult, 0, len(albumIDs))
	resultCh := make(chan AlbumBatchResult, len(albumIDs))
	jobCh := make(chan string)
	var wg sync.WaitGroup

	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			d := NewDownloader(option)
			for aid := range jobCh {
				album, err := d.DownloadAlbum(aid)
				if err == nil {
					if e2 := d.RaiseIfHasFailures(); e2 != nil {
						err = e2
					}
				}
				resultCh <- AlbumBatchResult{ID: aid, Album: album, Err: err}
			}
		}()
	}

	for _, aid := range albumIDs {
		jobCh <- aid
	}
	close(jobCh)
	wg.Wait()
	close(resultCh)

	for r := range resultCh {
		results = append(results, r)
	}
	return results
}

// FetchAlbumDetailsBatch 并发获取多个 album 详情（不下载图片）。
func FetchAlbumDetailsBatch(client *Client, albumIDs []string, workers int) []AlbumBatchResult {
	if client == nil {
		return []AlbumBatchResult{{Err: fmt.Errorf("nil client")}}
	}
	if workers <= 0 {
		workers = 1
	}
	results := make([]AlbumBatchResult, 0, len(albumIDs))
	resultCh := make(chan AlbumBatchResult, len(albumIDs))
	jobCh := make(chan string)
	var wg sync.WaitGroup

	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for aid := range jobCh {
				album, err := client.GetAlbumDetail(aid)
				resultCh <- AlbumBatchResult{ID: aid, Album: album, Err: err}
			}
		}()
	}

	for _, aid := range albumIDs {
		jobCh <- aid
	}
	close(jobCh)
	wg.Wait()
	close(resultCh)

	for r := range resultCh {
		results = append(results, r)
	}
	return results
}

// FetchPhotoDetailsBatch 并发获取多个 photo 详情。
func FetchPhotoDetailsBatch(client *Client, photoIDs []string, workers int, fetchAlbum, fetchScrambleID bool) []PhotoBatchResult {
	if client == nil {
		return []PhotoBatchResult{{Err: fmt.Errorf("nil client")}}
	}
	if workers <= 0 {
		workers = 1
	}
	results := make([]PhotoBatchResult, 0, len(photoIDs))
	resultCh := make(chan PhotoBatchResult, len(photoIDs))
	jobCh := make(chan string)
	var wg sync.WaitGroup

	for i := 0; i < workers; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for pid := range jobCh {
				photo, err := client.GetPhotoDetail(pid, fetchAlbum, fetchScrambleID)
				resultCh <- PhotoBatchResult{ID: pid, Photo: photo, Err: err}
			}
		}()
	}

	for _, pid := range photoIDs {
		jobCh <- pid
	}
	close(jobCh)
	wg.Wait()
	close(resultCh)

	for r := range resultCh {
		results = append(results, r)
	}
	return results
}

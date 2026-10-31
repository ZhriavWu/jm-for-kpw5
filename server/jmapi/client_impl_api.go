package jmapi

func (a *apiImpl) GetAlbumDetail(albumID string) (*AlbumDetail, error) {
	return a.c.apiGetAlbumDetail(albumID)
}

func (a *apiImpl) GetPhotoDetail(photoID string, fetchAlbum bool, fetchScrambleID bool) (*PhotoDetail, error) {
	return a.c.apiGetPhotoDetail(photoID, fetchAlbum, fetchScrambleID)
}

func (a *apiImpl) CheckPhoto(photo *PhotoDetail) error {
	return a.c.apiCheckPhoto(photo)
}

func (a *apiImpl) GetScrambleID(photoID string) (string, error) {
	return a.c.apiGetScrambleID(photoID)
}

func (a *apiImpl) Search(searchQuery string, page int, mainTag int, orderBy, timeRange, category, subCategory string) (*SearchResult, error) {
	return a.c.apiSearch(searchQuery, page, mainTag, orderBy, timeRange, category, subCategory)
}

func (a *apiImpl) CategoriesFilter(page int, timeRange, category, orderBy, subCategory string) (*SearchResult, error) {
	return a.c.apiCategoriesFilter(page, timeRange, category, orderBy, subCategory)
}

func (a *apiImpl) Setting() (map[string]any, error) {
	return a.c.apiSetting()
}

func (a *apiImpl) Login(username, password string) (map[string]any, error) {
	return a.c.apiLogin(username, password)
}

func (a *apiImpl) FavoriteFolder(page int, orderBy, folderID, username string) (*FavoriteResult, error) {
	return a.c.apiFavoriteFolder(page, orderBy, folderID, username)
}

func (a *apiImpl) AddFavoriteAlbum(albumID, folderID string) (map[string]any, error) {
	return a.c.apiAddFavoriteAlbum(albumID, folderID)
}

func (a *apiImpl) AlbumComment(videoID, comment, originator, status, commentID string) (map[string]any, error) {
	return a.c.apiAlbumComment(videoID, comment, originator, status, commentID)
}

func (a *apiImpl) DownloadImage(imgURL string) ([]byte, error) {
	return a.c.apiDownloadImage(imgURL)
}

func (a *apiImpl) DownloadByImageDetail(photoID, imageName string) ([]byte, error) {
	return a.c.apiDownloadByImageDetail(photoID, imageName)
}

func (a *apiImpl) DownloadAlbumCover(albumID string) ([]byte, error) {
	return a.c.apiDownloadAlbumCover(albumID)
}

func (a *apiImpl) AutoUpdateDomains() error {
	return a.c.apiAutoUpdateDomains()
}


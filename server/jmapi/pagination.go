package jmapi

import "fmt"

type SearchParams struct {
	Query       string
	MainTag     int
	Page        int
	OrderBy     string
	TimeRange   string
	Category    string
	SubCategory string
}

type CategoryParams struct {
	Page        int
	TimeRange   string
	Category    string
	OrderBy     string
	SubCategory string
}

type FavoriteParams struct {
	Page     int
	OrderBy  string
	FolderID string
	Username string
}

// SearchPages 按页遍历搜索结果。handler 返回 false 可提前终止。
func (c *Client) SearchPages(params SearchParams, handler func(page int, result *SearchResult) (bool, error)) error {
	if handler == nil {
		return fmt.Errorf("handler is nil")
	}
	if params.Page <= 0 {
		params.Page = 1
	}
	for page := params.Page; ; page++ {
		res, err := c.Search(params.Query, page, params.MainTag, params.OrderBy, params.TimeRange, params.Category, params.SubCategory)
		if err != nil {
			return err
		}
		cont, err := handler(page, res)
		if err != nil {
			return err
		}
		if !cont || res == nil || len(res.Items) == 0 {
			return nil
		}
	}
}

// CategoriesPages 按页遍历分类结果。
func (c *Client) CategoriesPages(params CategoryParams, handler func(page int, result *SearchResult) (bool, error)) error {
	if handler == nil {
		return fmt.Errorf("handler is nil")
	}
	if params.Page <= 0 {
		params.Page = 1
	}
	for page := params.Page; ; page++ {
		res, err := c.CategoriesFilter(page, params.TimeRange, params.Category, params.OrderBy, params.SubCategory)
		if err != nil {
			return err
		}
		cont, err := handler(page, res)
		if err != nil {
			return err
		}
		if !cont || res == nil || len(res.Items) == 0 {
			return nil
		}
	}
}

// FavoritePages 按页遍历收藏夹。
func (c *Client) FavoritePages(params FavoriteParams, handler func(page int, result *FavoriteResult) (bool, error)) error {
	if handler == nil {
		return fmt.Errorf("handler is nil")
	}
	if params.Page <= 0 {
		params.Page = 1
	}
	for page := params.Page; ; page++ {
		res, err := c.FavoriteFolder(page, params.OrderBy, params.FolderID, params.Username)
		if err != nil {
			return err
		}
		cont, err := handler(page, res)
		if err != nil {
			return err
		}
		if !cont || res == nil || len(res.Items) == 0 {
			return nil
		}
	}
}

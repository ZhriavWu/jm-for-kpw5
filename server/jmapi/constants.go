package jmapi

const (
	DefaultAppVersion      = "2.0.19"
	AppTokenSecret         = "18comicAPP"
	AppTokenSecret2        = "18comicAPPContent"
	AppDataSecret          = "185Hcomic3PAPP7R"
	APIDomainServerSecret  = "diosfjckwpqpdfjkvnqQjsik"
	DefaultHTMLUserAgent   = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
	DefaultMobileUserAgent = "Mozilla/5.0 (Linux; Android 9; V1938CT Build/PQ3A.190705.11211812; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36"
)

// 排序
const (
	OrderByLatest  = "mr"
	OrderByView    = "mv"
	OrderByPicture = "mp"
	OrderByLike    = "tf"
	OrderByScore   = "tr"
	OrderByComment = "md"
)

// 时间范围
const (
	TimeToday = "t"
	TimeWeek  = "w"
	TimeMonth = "m"
	TimeAll   = "a"
)

// 分类
const (
	CategoryAll          = "0"
	CategoryDoujin       = "doujin"
	CategorySingle       = "single"
	CategoryShort        = "short"
	CategoryAnother      = "another"
	CategoryHanman       = "hanman"
	CategoryMeiman       = "meiman"
	CategoryDoujinCos    = "doujin_cosplay"
	Category3D           = "3D"
	CategoryEnglishSite  = "english_site"
)

var DefaultAPIDomains = []string{
	"www.cdnaspa.vip",
	"www.cdnaspa.club",
	"www.cdnplaystation6.vip",
	"www.cdnplaystation6.cc",
}

var DefaultHTMLDomains = []string{
	"18comic.vip",
}

var DefaultAPIDomainServerURLs = []string{
	"https://rup4a04-c01.tos-ap-southeast-1.bytepluses.com/newsvr-2025.txt",
	"https://rup4a04-c02.tos-cn-hongkong.bytepluses.com/newsvr-2025.txt",
}

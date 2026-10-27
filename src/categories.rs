//! 文件分类（按扩展名）。
//!
//! 分类判定是热路径：搜索与计数每帧可能遍历上百万条目，
//! 因此全程零分配（用 `eq_ignore_ascii_case` 代替 `to_ascii_lowercase` + 线性 contains）。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    All,
    Image,
    Doc,
    Video,
    Audio,
    Phone,
    Dir,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::All,
        Category::Image,
        Category::Doc,
        Category::Video,
        Category::Audio,
        Category::Phone,
        Category::Dir,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Category::All => "全部",
            Category::Image => "图片",
            Category::Doc => "文档",
            Category::Video => "视频",
            Category::Audio => "音频",
            Category::Phone => "手机",
            Category::Dir => "目录",
        }
    }

    /// 在计数数组中的下标（与 `ALL` 顺序一致）。
    pub fn index(&self) -> usize {
        match self {
            Category::All => 0,
            Category::Image => 1,
            Category::Doc => 2,
            Category::Video => 3,
            Category::Audio => 4,
            Category::Phone => 5,
            Category::Dir => 6,
        }
    }
}

const IMAGE_EXT: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "ico", "tif", "tiff", "heic", "heif", "avif", "raw", "cr2",
    "cr3", "nef", "arw", "dng", "psd", "xcf", "wmf", "emf",
];
const DOC_EXT: &[&str] = &[
    "txt", "md", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pdf", "csv", "tsv", "rtf", "odt", "ods", "odp", "epub",
    "mobi", "azw3", "djvu", "chm", "hwp", "wps", "et", "dps", "log", "ini", "cfg", "json", "xml", "yml", "yaml",
    "html", "htm", "css",
];
const VIDEO_EXT: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "flv", "m4v", "webm", "ts", "m2ts", "rmvb", "rm", "mpg", "mpeg", "3gp", "vob",
    "ogv", "asf", "divx",
];
const AUDIO_EXT: &[&str] = &[
    "mp3", "wav", "flac", "aac", "ogg", "oga", "m4a", "wma", "ape", "opus", "amr", "mid", "midi", "mka", "ac3", "dts",
];
const PHONE_EXT: &[&str] = &["apk", "xapk", "apks", "ipa", "appx", "msix", "appxbundle", "msixbundle", "ipcc", "ipsw"];

pub fn ext_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i + 1..],
        _ => "",
    }
}

#[inline]
fn hit(list: &[&str], ext: &str) -> bool {
    list.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// 分类判定。返回 None 表示只属于"全部"。全程无堆分配。
pub fn classify(name: &str, is_dir: bool) -> Option<Category> {
    if is_dir {
        return Some(Category::Dir);
    }
    let ext = ext_of(name);
    if ext.is_empty() {
        return None;
    }
    if hit(IMAGE_EXT, ext) {
        Some(Category::Image)
    } else if hit(DOC_EXT, ext) {
        Some(Category::Doc)
    } else if hit(VIDEO_EXT, ext) {
        Some(Category::Video)
    } else if hit(AUDIO_EXT, ext) {
        Some(Category::Audio)
    } else if hit(PHONE_EXT, ext) {
        Some(Category::Phone)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_basic() {
        assert_eq!(classify("a.jpg", false), Some(Category::Image));
        assert_eq!(classify("A.JPEG", false), Some(Category::Image));
        assert_eq!(classify("b.mp4", false), Some(Category::Video));
        assert_eq!(classify("c.mp3", false), Some(Category::Audio));
        assert_eq!(classify("d.apk", false), Some(Category::Phone));
        assert_eq!(classify("e.pdf", false), Some(Category::Doc));
        assert_eq!(classify("f", true), Some(Category::Dir));
        assert_eq!(classify("g.xyz", false), None);
        assert_eq!(ext_of("noext"), "");
        assert_eq!(ext_of("a.b.c"), "c");
    }
}

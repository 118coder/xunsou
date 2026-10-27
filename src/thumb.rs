//! Windows Shell 缩略图。
//!
//! 走 `IShellItemImageFactory`：与资源管理器同一条管线，视频取首帧，
//! PDF / Office / HEIC 等由系统注册的缩略图处理器负责，命中缩略图缓存时
//! 几乎没有磁盘 IO。这样不必为每种容器格式各引一套解码器。
//!
//! 失败（没有注册处理器、文件损坏、权限不足）时返回 `None`，由调用方
//! 决定降级到文本预览还是不显示——不会返回通用文件图标。

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS,
    HBITMAP, HGDIOBJ,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows_sys::Win32::UI::Shell::SHCreateItemFromParsingName;

const IID_ISHELLITEM: GUID = GUID {
    data1: 0x4382_6d1e,
    data2: 0xe718,
    data3: 0x42ee,
    data4: [0xbc, 0x55, 0xa1, 0xe2, 0x61, 0xc3, 0x7b, 0xfe],
};
const IID_ISHELLITEMIMAGEFACTORY: GUID = GUID {
    data1: 0xbcc1_8b79,
    data2: 0xba16,
    data3: 0x442f,
    data4: [0x80, 0xc4, 0x8a, 0x59, 0xc3, 0x0c, 0x46, 0x3b],
};

/// 允许系统返回比请求更大的图（避免为凑尺寸而失真）
const SIIGBF_BIGGERSIZEOK: i32 = 0x01;
/// 只接受真正的缩略图，不接受通用图标占位
const SIIGBF_THUMBNAILONLY: i32 = 0x08;
/// BI_RGB
const BI_RGB: u32 = 0;

#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}

#[repr(C)]
struct UnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ImageFactoryVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_image: unsafe extern "system" fn(*mut c_void, Size, i32, *mut HBITMAP) -> i32,
}

/// 取系统缩略图，返回 `(RGBA 像素, 宽, 高)`。最长边不超过 `max`。
pub fn shell_thumbnail(path: &str, max: u32) -> Option<(Vec<u8>, usize, usize)> {
    let wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        // MTA 足够：缩略图处理器不需要消息泵。已初始化时返回 S_FALSE，同样忽略。
        let hr = CoInitializeEx(null(), COINIT_MULTITHREADED as u32);
        let out = inner(&wide, max);
        if hr >= 0 {
            CoUninitialize();
        }
        out
    }
}

unsafe fn inner(wide: &[u16], max: u32) -> Option<(Vec<u8>, usize, usize)> {
    let mut item: *mut c_void = null_mut();
    if SHCreateItemFromParsingName(wide.as_ptr(), null_mut(), &IID_ISHELLITEM, &mut item) < 0 || item.is_null() {
        return None;
    }
    let uvtbl = *(item as *const *const UnknownVtbl);
    let qi = (*uvtbl).query_interface;
    let rel = (*uvtbl).release;

    let mut fac: *mut c_void = null_mut();
    let hr = qi(item, &IID_ISHELLITEMIMAGEFACTORY, &mut fac);
    rel(item);
    if hr < 0 || fac.is_null() {
        return None;
    }

    let fvtbl = *(fac as *const *const ImageFactoryVtbl);
    let get_image = (*fvtbl).get_image;
    let release_f = (*fvtbl).release;

    let mut hbm: HBITMAP = null_mut();
    let hr = get_image(
        fac,
        Size { cx: max as i32, cy: max as i32 },
        SIIGBF_BIGGERSIZEOK | SIIGBF_THUMBNAILONLY,
        &mut hbm,
    );
    release_f(fac);
    if hr < 0 || hbm.is_null() {
        return None;
    }

    let out = hbitmap_to_rgba(hbm);
    DeleteObject(hbm as HGDIOBJ);
    out
}

unsafe fn hbitmap_to_rgba(hbm: HBITMAP) -> Option<(Vec<u8>, usize, usize)> {
    let mut bmp: BITMAP = std::mem::zeroed();
    if GetObjectW(hbm as HGDIOBJ, size_of::<BITMAP>() as i32, &mut bmp as *mut _ as *mut c_void) == 0 {
        return None;
    }
    let (w, h) = (bmp.bmWidth, bmp.bmHeight);
    if w <= 0 || h <= 0 || w > 8192 || h > 8192 {
        return None;
    }

    let hdc = GetDC(null_mut());
    if hdc.is_null() {
        return None;
    }
    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = w;
    bi.bmiHeader.biHeight = -h; // 负高度 = 自上而下，省一次翻转
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;

    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    let lines = GetDIBits(
        hdc,
        hbm,
        0,
        h as u32,
        buf.as_mut_ptr() as *mut c_void,
        &mut bi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(null_mut(), hdc);
    if lines == 0 {
        return None;
    }

    // BGRA → RGBA
    let mut has_alpha = false;
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
        if px[3] != 0 {
            has_alpha = true;
        }
    }
    if has_alpha {
        // 系统缩略图的 alpha 是预乘的，egui 按非预乘接收，这里还原
        for px in buf.chunks_exact_mut(4) {
            let a = px[3];
            if a != 0 && a != 255 {
                let inv = 255.0 / a as f32;
                px[0] = (px[0] as f32 * inv).min(255.0) as u8;
                px[1] = (px[1] as f32 * inv).min(255.0) as u8;
                px[2] = (px[2] as f32 * inv).min(255.0) as u8;
            }
        }
    } else {
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    Some((buf, w as usize, h as usize))
}

#[cfg(test)]
mod probe {
    use super::*;

    /// 用系统自带壁纸验证整条 COM → HBITMAP → RGBA 链路。
    /// 视频首帧走的是同一个 `IShellItemImageFactory::GetImage`，
    /// 图片能出图即证明 vtable 调用、DIB 转换与 alpha 处理都正确。
    #[test]
    fn shell_thumbnail_reads_real_image() {
        // 用 Path 逐级拼接，分隔符取自 MAIN_SEPARATOR，避免手写字面量被工具链改写
        let root = format!("C:{}", std::path::MAIN_SEPARATOR);
        let p = std::path::Path::new(&root)
            .join("Windows")
            .join("Web")
            .join("Wallpaper")
            .join("Windows")
            .join("img0.jpg");
        if !p.exists() {
            return;
        }
        let (rgba, w, h) = shell_thumbnail(&p.to_string_lossy(), 512).expect("系统缩略图应可用");
        assert_eq!(rgba.len(), w * h * 4, "像素缓冲长度应与尺寸一致");
        assert!(w >= 64 && h >= 64, "缩略图尺寸异常: {w}x{h}");
        // 至少要有几种不同颜色，否则说明拿到的是空图或占位图
        let mut uniq = std::collections::HashSet::new();
        for px in rgba.chunks_exact(4).step_by(97) {
            uniq.insert((px[0], px[1], px[2]));
        }
        assert!(uniq.len() > 16, "图像内容过于单一，可能是占位图");
        assert!(rgba.chunks_exact(4).all(|px| px[3] == 255), "不透明图片的 alpha 应被归一为 255");
    }

    /// 文件不存在时必须安静地返回 None，而不是崩溃或返回图标占位。
    #[test]
    fn shell_thumbnail_missing_file_is_none() {
        let root = format!("C:{}", std::path::MAIN_SEPARATOR);
        let missing = std::path::Path::new(&root).join("__xunsou_no_such_media__.mp4");
        assert!(shell_thumbnail(&missing.to_string_lossy(), 256).is_none());
    }
}


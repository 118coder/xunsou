//! Win32 FFI 辅助：卷枚举、管理员检测、MFT/USN 原始接口、全局热键、提权重启。

#![allow(non_snake_case, clippy::missing_safety_doc)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileTimeToLocalFileTime, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN};
use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, SW_SHOWNORMAL, WM_HOTKEY, WM_QUIT,
};

pub use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
pub use windows_sys::Win32::Storage::FileSystem::{FILE_BEGIN, ReadFile, SetFilePointerEx};

pub const DRIVE_FIXED: u32 = 3;
pub const FSCTL_GET_NTFS_VOLUME_DATA: u32 = 0x90064;
pub const FSCTL_GET_RETRIEVAL_POINTERS: u32 = 0x90073;
pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x900F4;
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x900BB;

// USN 原因位（winnt.h）
pub const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
pub const USN_REASON_DATA_EXTEND: u32 = 0x0000_0002;
pub const USN_REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
pub const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
pub const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
pub const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
pub const USN_REASON_CLOSE: u32 = 0x8000_0000;

pub fn to_wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn is_admin() -> bool {
    unsafe { IsUserAnAdmin() != 0 }
}

/// 当前时间的 FILETIME 值。
pub fn now_filetime() -> u64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() + 11_644_473_600) * 10_000_000 + (d.subsec_nanos() / 100) as u64
}

/// FILETIME(UTC) → FILETIME(本地时区)，含夏令时。失败时原样返回。
pub fn filetime_to_local(ft: u64) -> u64 {
    if ft == 0 {
        return 0;
    }
    let src = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut dst = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let ok = unsafe { FileTimeToLocalFileTime(&src, &mut dst) };
    if ok == 0 {
        return ft;
    }
    ((dst.dwHighDateTime as u64) << 32) | dst.dwLowDateTime as u64
}

/// 把文件/目录移入回收站。
///
/// 走 `SHFileOperationW(FO_DELETE | FOF_ALLOWUNDO)`：系统级回收站语义，
/// 误删可从回收站还原。一次调用可携带多个路径（双 NUL 结尾）。
pub fn delete_to_recycle_bin(paths: &[String]) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FO_DELETE, SHFILEOPSTRUCTW,
    };
    if paths.is_empty() {
        return Ok(());
    }
    // pFrom 要求：每项一个 NUL，整体再补一个 NUL
    let mut buf: Vec<u16> = Vec::new();
    for p in paths {
        buf.extend(p.encode_utf16());
        buf.push(0);
    }
    buf.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: buf.as_ptr(),
        pTo: std::ptr::null(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    let code = unsafe { SHFileOperationW(&mut op) };
    if code == 0 && op.fAnyOperationsAborted == 0 {
        Ok(())
    } else {
        Err(format!("0x{code:08X}"))
    }
}

/// 枚举所有本地固定磁盘盘符。
pub fn fixed_drives() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26 {
        if mask & (1 << i) != 0 {
            let letter = (b'A' + i) as char;
            let root = format!("{letter}:\\");
            if unsafe { GetDriveTypeW(to_wide(&root).as_ptr()) } == DRIVE_FIXED {
                out.push(letter);
            }
        }
    }
    out
}

/// 返回 (序列号, 文件系统名)。失败返回 None。
pub fn volume_info(letter: char) -> Option<(u64, String)> {
    let root = to_wide(&format!("{letter}:\\"));
    let mut serial: u32 = 0;
    let mut fs_buf = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            &mut serial,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };
    if ok == 0 {
        None
    } else {
        Some((serial as u64, from_wide(&fs_buf)))
    }
}

pub fn open_volume(letter: char) -> Option<HANDLE> {
    let path = to_wide(&format!("\\\\.\\{letter}:"));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            0x8000_0000, // GENERIC_READ
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() { None } else { Some(h) }
}

pub fn close_handle(h: HANDLE) {
    unsafe { CloseHandle(h); }
}

/// DeviceIoControl 封装：返回实际输出字节数。
pub fn device_io(h: HANDLE, code: u32, in_buf: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut ret: u32 = 0;
    let in_ptr = if in_buf.is_empty() {
        std::ptr::null()
    } else {
        in_buf.as_ptr() as *const core::ffi::c_void
    };
    let ok = unsafe {
        windows_sys::Win32::System::IO::DeviceIoControl(
            h,
            code,
            in_ptr,
            in_buf.len() as u32,
            out.as_mut_ptr() as _,
            out.len() as u32,
            &mut ret,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { None } else { Some(ret as usize) }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct NtfsVolumeData {
    pub volume_serial: i64,
    pub number_sectors: i64,
    pub total_clusters: i64,
    pub free_clusters: i64,
    pub total_reserved: i64,
    pub bytes_per_sector: u32,
    pub bytes_per_cluster: u32,
    pub bytes_per_file_record: u32,
    pub clusters_per_file_record: u32,
    pub mft_valid_data_length: i64,
    pub mft_start_lcn: i64,
    pub mft2_start_lcn: i64,
    pub mft_zone_start: i64,
    pub mft_zone_end: i64,
}

pub fn query_ntfs_volume_data(h: HANDLE) -> Option<NtfsVolumeData> {
    let mut out = [0u8; std::mem::size_of::<NtfsVolumeData>() + 32];
    let n = device_io(h, FSCTL_GET_NTFS_VOLUME_DATA, &[], &mut out)?;
    if n < std::mem::size_of::<NtfsVolumeData>() {
        return None;
    }
    // NTFS_VOLUME_DATA_BUFFER 字段布局与上面结构一致（前 72 字节）
    let v = NtfsVolumeData::default();
    let sz = std::mem::size_of_val(&v);
    let p = out.as_ptr();
    unsafe {
        Some(NtfsVolumeData {
            volume_serial: std::ptr::read_unaligned(p as *const i64),
            number_sectors: std::ptr::read_unaligned(p.add(8) as *const i64),
            total_clusters: std::ptr::read_unaligned(p.add(16) as *const i64),
            free_clusters: std::ptr::read_unaligned(p.add(24) as *const i64),
            total_reserved: std::ptr::read_unaligned(p.add(32) as *const i64),
            bytes_per_sector: std::ptr::read_unaligned(p.add(40) as *const u32),
            bytes_per_cluster: std::ptr::read_unaligned(p.add(44) as *const u32),
            bytes_per_file_record: std::ptr::read_unaligned(p.add(48) as *const u32),
            clusters_per_file_record: std::ptr::read_unaligned(p.add(52) as *const u32),
            mft_valid_data_length: std::ptr::read_unaligned(p.add(56) as *const i64),
            mft_start_lcn: std::ptr::read_unaligned(p.add(64) as *const i64),
            mft2_start_lcn: std::ptr::read_unaligned(p.add(72) as *const i64),
            mft_zone_start: std::ptr::read_unaligned(p.add(80) as *const i64),
            mft_zone_end: std::ptr::read_unaligned(p.add(88) as *const i64),
        })
    }
    .filter(|_| sz > 0)
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsnJournalData {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub max_usn: i64,
    pub maximum_size: u64,
    pub allocation_delta: u64,
}

pub fn query_usn_journal(h: HANDLE) -> Option<UsnJournalData> {
    let mut out = [0u8; 64];
    let n = device_io(h, FSCTL_QUERY_USN_JOURNAL, &[], &mut out)?;
    if n < 48 {
        return None;
    }
    let p = out.as_ptr();
    unsafe {
        Some(UsnJournalData {
            journal_id: std::ptr::read_unaligned(p as *const u64),
            first_usn: std::ptr::read_unaligned(p.add(8) as *const i64),
            next_usn: std::ptr::read_unaligned(p.add(16) as *const i64),
            lowest_valid_usn: std::ptr::read_unaligned(p.add(24) as *const i64),
            max_usn: std::ptr::read_unaligned(p.add(32) as *const i64),
            maximum_size: std::ptr::read_unaligned(p.add(40) as *const u64),
            allocation_delta: std::ptr::read_unaligned(p.add(48) as *const u64),
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ReadUsnJournalDataV0 {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
}

/// 读取一批 USN 记录。返回 (next_usn, 原始记录缓冲区)。
pub fn read_usn_records(h: HANDLE, journal_id: u64, from_usn: i64, buf: &mut Vec<u8>) -> Option<(i64, usize)> {
    let input = ReadUsnJournalDataV0 {
        start_usn: from_usn,
        reason_mask: 0xFFFF_FFFF,
        return_only_on_close: 0,
        timeout: 0,
        bytes_to_wait_for: 0,
        usn_journal_id: journal_id,
    };
    let in_bytes = unsafe {
        std::slice::from_raw_parts(
            &input as *const ReadUsnJournalDataV0 as *const u8,
            std::mem::size_of::<ReadUsnJournalDataV0>(),
        )
    };
    if buf.len() < 1 << 20 {
        buf.resize(1 << 20, 0);
    }
    let n = device_io(h, FSCTL_READ_USN_JOURNAL, in_bytes, buf)?;
    if n < 8 {
        return None;
    }
    let next = i64::from_le_bytes(buf[0..8].try_into().ok()?);
    Some((next, n - 8))
}

/// 读取 $MFT 的磁盘 run 列表 (逻辑字节偏移, 物理字节偏移, 长度)。
pub fn mft_runs(h: HANDLE, cluster: u32, mft_len: u64) -> Option<Vec<(u64, u64, u64)>> {
    let mut in_buf = [0u8; 8]; // STARTING_VCN_INPUT_BUFFER { 0 }
    let mut out = vec![0u8; 64 * 1024];
    let n = device_io(h, FSCTL_GET_RETRIEVAL_POINTERS, &in_buf, &mut out)?;
    if n < 16 {
        return None;
    }
    let extent_count = u32::from_le_bytes(out[0..4].try_into().ok()?) as usize;
    let _start_vcn = i64::from_le_bytes(out[8..16].try_into().ok()?);
    let mut runs = Vec::new();
    let mut cur_vcn: i64 = 0;
    let mut logical: u64 = 0;
    let total_vcn = (mft_len + cluster as u64 - 1) / cluster as u64;
    for i in 0..=extent_count {
        let off = 16 + i * 16;
        if off + 16 > n {
            break;
        }
        let next_vcn = i64::from_le_bytes(out[off..off + 8].try_into().ok()?);
        let lcn = i64::from_le_bytes(out[off + 8..off + 16].try_into().ok()?);
        if next_vcn <= cur_vcn {
            break;
        }
        let len_clusters = (next_vcn - cur_vcn) as u64;
        if lcn >= 0 {
            let phys = lcn as u64 * cluster as u64;
            runs.push((logical, phys, len_clusters * cluster as u64));
        }
        logical += len_clusters * cluster as u64;
        cur_vcn = next_vcn;
        if cur_vcn as u64 >= total_vcn {
            break;
        }
    }
    if logical < mft_len {
        // run 覆盖不足（存在稀疏段），尽量用已有的
    }
    Some(runs)
}

/// 顺序读取 $MFT，把连续完整记录批量回调（可变切片，便于就地修复 fixup）。sink(起始记录号, 记录字节)。
///
/// 注意：记录号由「逻辑偏移 / 记录大小」推导，而不是按已读字节累计。
/// $MFT 的 run 列表可能出现空洞（稀疏段 / 负 LCN），累计法会让空洞之后的
/// 所有记录编号整体漂移，导致 FRN 与文件错配。
pub fn read_mft_records(
    h: HANDLE,
    runs: &[(u64, u64, u64)],
    record_size: usize,
    mft_len: u64,
    mut sink: impl FnMut(u64, &mut [u8]),
) -> std::io::Result<()> {
    if record_size == 0 {
        return Ok(());
    }
    let chunk = 4usize << 20;
    let mut buf = vec![0u8; chunk];
    let rs = record_size as u64;

    for &(log_off, phys_off, len) in runs {
        // run 起点未按记录对齐时跳过头部残余（正常 NTFS 恒为 0）
        let misalign = (log_off % rs) as u64;
        if misalign >= len {
            continue;
        }
        let mut carry: Vec<u8> = Vec::new();
        let mut record_no = log_off / rs;
        let mut done = misalign;
        while done < len {
            let want = ((len - done) as usize).min(chunk);
            let mut moved: i64 = 0;
            let ok = unsafe { SetFilePointerEx(h, (phys_off + done) as i64, &mut moved, FILE_BEGIN) };
            if ok == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut got: u32 = 0;
            let ok = unsafe {
                ReadFile(
                    h,
                    buf.as_mut_ptr() as _,
                    want as u32,
                    &mut got,
                    std::ptr::null_mut::<OVERLAPPED>() as *mut OVERLAPPED,
                )
            };
            if ok == 0 || got == 0 {
                return Err(std::io::Error::last_os_error());
            }
            carry.extend_from_slice(&buf[..got as usize]);

            let mut off = 0usize;
            while off + record_size <= carry.len() {
                let logical_pos = record_no * rs;
                if logical_pos >= mft_len {
                    return Ok(());
                }
                let avail = (carry.len() - off) / record_size;
                let remaining = ((mft_len - logical_pos) / rs) as usize;
                let n = avail.min(remaining);
                if n == 0 {
                    return Ok(());
                }
                let start = record_no;
                let bytes = &mut carry[off..off + n * record_size];
                record_no += n as u64;
                off += n * record_size;
                sink(start, bytes);
            }
            carry.drain(..off);
            done += got as u64;
        }
        // 跨 run 的残片理论上不存在（run 与记录均按簇对齐），直接丢弃
    }
    Ok(())
}

// ---------- 全局热键 ----------

pub struct HotkeyThread {
    pub thread_id: u32,
}

impl HotkeyThread {
    /// 启动热键线程。返回 (线程句柄, 消息接收端)。
    pub fn start(desc: &str) -> Option<(HotkeyThread, std::sync::mpsc::Receiver<()>)> {
        let (mods, vk) = parse_hotkey(desc)?;
        let (tx, rx) = std::sync::mpsc::channel();
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let repaint_tx = tx.clone();
        let handle = std::thread::Builder::new()
            .name("hotkey".into())
            .spawn(move || unsafe {
                let tid = windows_sys::Win32::System::Threading::GetCurrentThreadId();
                let _ = tid_tx.send(tid);
                if vk != 0 {
                    RegisterHotKey(std::ptr::null_mut(), 1, mods | MOD_NOREPEAT, vk);
                }
                let mut msg: MSG = std::mem::zeroed();
                loop {
                    let r = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                    if r <= 0 {
                        break;
                    }
                    if msg.message == WM_HOTKEY && msg.wParam == 1 {
                        let _ = repaint_tx.send(());
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            })
            .ok()?;
        let tid = tid_rx.recv_timeout(std::time::Duration::from_secs(2)).ok()?;
        std::mem::forget(handle); // 线程常驻，由 WM_QUIT 退出
        Some((HotkeyThread { thread_id: tid }, rx))
    }

    pub fn stop(&self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
    }
}

/// 解析 "Alt + `" / "Ctrl + Shift + F1" 之类的热键描述。
pub fn parse_hotkey(desc: &str) -> Option<(u32, u32)> {
    let mut mods = 0u32;
    let mut key: Option<u32> = None;
    for part in desc.split('+') {
        let p = part.trim().to_ascii_lowercase();
        match p.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" | "windows" => mods |= MOD_WIN,
            "" => continue,
            s => {
                key = Some(special_vk(s).or_else(|| {
                    let mut chars = s.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    let r = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW(c as u16) };
                    if r == -1 {
                        return None;
                    }
                    Some((r & 0xFF) as u32)
                })?);
            }
        }
    }
    let vk = key?;
    if vk == 0 {
        return None;
    }
    Some((mods, vk))
}

fn special_vk(s: &str) -> Option<u32> {
    let s = s.to_ascii_uppercase();
    if let Some(rest) = s.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    match s.as_str() {
        "ESC" | "ESCAPE" => Some(0x1B),
        "TAB" => Some(0x09),
        "SPACE" => Some(0x20),
        "ENTER" | "RETURN" => Some(0x0D),
        "BACKSPACE" => Some(0x08),
        "DELETE" | "DEL" => Some(0x2E),
        "INSERT" | "INS" => Some(0x2D),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" | "PGUP" => Some(0x21),
        "PAGEDOWN" | "PGDN" => Some(0x22),
        "OEM_3" | "`" | "~" => Some(0xC0),
        _ => None,
    }
}

/// 以管理员身份重启自身。
pub fn restart_elevated() {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_w = to_wide(&exe.as_os_str().to_string_lossy());
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            to_wide("runas").as_ptr(),
            exe_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL as i32,
        );
    }
}

/// 在资源管理器中定位文件。
pub fn show_in_explorer(path: &str) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
}

/// 便捷：OsString → utf8 lossy。
pub fn os_to_string(s: &OsString) -> String {
    s.to_string_lossy().into_owned()
}

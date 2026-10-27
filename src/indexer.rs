//! 索引器：NTFS MFT 直读扫描、USN 日志实时增量、目录遍历回退、总协调线程。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;

use crate::config;
use crate::model::{load_snapshot, save_snapshot, EntryRecord, Index, NO_PARENT, FLAG_DIR};
use crate::win;

// ============================ 状态共享 ============================

#[derive(Clone)]
pub struct StatusHandle {
    st: Arc<Mutex<Status>>,
    ctx: egui::Context,
}

pub struct Status {
    pub phase: String,
    pub ready: bool,
    pub volumes_done: usize,
    pub volumes_total: usize,
    pub error: Option<String>,
    pub needs_rescan: bool,
    pub mft_mode: bool,
    pub started: Instant,
}

impl StatusHandle {
    pub fn set(&self, f: impl FnOnce(&mut Status)) {
        {
            let mut g = self.st.lock().unwrap();
            f(&mut g);
        }
        self.ctx.request_repaint();
    }
    pub fn get(&self) -> Status {
        self.st.lock().unwrap().clone()
    }
    /// 测试/无头模式用：给定初始状态构造。
    pub fn new(ctx: egui::Context, st: Status) -> Self {
        Self { st: Arc::new(Mutex::new(st)), ctx }
    }
}

impl Clone for Status {
    fn clone(&self) -> Self {
        Status {
            phase: self.phase.clone(),
            ready: self.ready,
            volumes_done: self.volumes_done,
            volumes_total: self.volumes_total,
            error: self.error.clone(),
            needs_rescan: self.needs_rescan,
            mft_mode: self.mft_mode,
            started: self.started,
        }
    }
}

pub struct IndexHandle {
    pub index: Arc<RwLock<Index>>,
    pub status: StatusHandle,
    pub excl_tx: std::sync::mpsc::Sender<Vec<String>>,
    /// 手动刷新：通知协调线程重建全部索引
    pub refresh_tx: std::sync::mpsc::Sender<()>,
}

// ============================ MFT 解析 ============================

#[derive(Debug, Clone)]
pub struct MftRec {
    pub mft_no: u64,
    pub parent_frn: u64,
    pub name: String,
    pub size: u64,
    pub mtime: u64,
    pub is_dir: bool,
}

#[inline]
fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes(s.try_into().unwrap()))
}
#[inline]
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}
#[inline]
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

pub const MFT_ROOT: u64 = 5;
const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// 解析一条 MFT 记录（1024B），就地应用更新序列号修复。
pub fn parse_mft_record(rec: &mut [u8], mft_no: u64) -> Option<MftRec> {
    if rec.len() < 64 || &rec[0..4] != b"FILE" {
        return None;
    }
    let usa_ofs = u16_at(rec, 4)? as usize;
    let usa_count = u16_at(rec, 6)? as usize;
    let attrs_ofs = u16_at(rec, 20)? as usize;
    let flags = u16_at(rec, 22)?;
    let base = u64_at(rec, 32)?;
    if base != 0 {
        return None; // 扩展记录：主记录已含所需属性
    }
    // 更新序列号修复
    if usa_count > 1 {
        let sector = rec.len() / (usa_count - 1);
        if sector == 0 || usa_ofs + usa_count * 2 > rec.len() {
            return None;
        }
        let sig = u16_at(rec, usa_ofs)?;
        for i in 1..usa_count {
            let pos = i * sector - 2;
            if pos + 2 > rec.len() {
                return None;
            }
            if rec[pos..pos + 2] != sig.to_le_bytes() {
                return None; // 校验失败，记录损坏
            }
            let val = u16_at(rec, usa_ofs + 2 + (i - 1) * 2)?;
            rec[pos..pos + 2].copy_from_slice(&val.to_le_bytes());
        }
    }
    let in_use = flags & 0x1 != 0;
    let is_dir = flags & 0x2 != 0;
    if !in_use {
        return None;
    }

    let mut si_mtime: u64 = 0;
    let mut fn_parent: u64 = 0;
    let mut fn_mtime: u64 = 0;
    let mut fn_name: Option<String> = None;
    let mut dos_name: Option<String> = None;
    let mut data_size: u64 = 0;

    let mut a = attrs_ofs;
    while a + 16 <= rec.len() {
        let t = match u32_at(rec, a) {
            Some(0xFFFF_FFFF) | None => break,
            Some(t) => t,
        };
        let len = match u32_at(rec, a + 4) {
            Some(l) if l as usize >= 24 && a + l as usize <= rec.len() => l as usize,
            _ => break,
        };
        let nonres = rec[a + 8];
        let aname_len = rec[a + 9] as usize;
        match (t, nonres) {
            (0x10, 0) if aname_len == 0 => {
                // $STANDARD_INFORMATION
                if let (Some(vl), Some(vo)) = (u32_at(rec, a + 16), u16_at(rec, a + 20)) {
                    let (vl, vo) = (vl as usize, vo as usize);
                    if vl >= 16 && a + vo + vl <= rec.len() {
                        si_mtime = u64_at(rec, a + vo + 8).unwrap_or(0);
                    }
                }
            }
            (0x30, 0) => {
                // $FILE_NAME
                if let (Some(vl), Some(vo)) = (u32_at(rec, a + 16), u16_at(rec, a + 20)) {
                    let (vl, vo) = (vl as usize, vo as usize);
                    if vl >= 66 && a + vo + vl <= rec.len() {
                        let vbase = a + vo;
                        let pref = u64_at(rec, vbase).unwrap_or(0);
                        let pmod = u64_at(rec, vbase + 16).unwrap_or(0);
                        let nl = rec[vbase + 64] as usize;
                        let ns = rec[vbase + 65];
                        let name_end = vbase + 66 + nl * 2;
                        if nl > 0 && name_end <= a + vo + vl {
                            let chars: Vec<u16> = rec[vbase + 66..name_end]
                                .chunks_exact(2)
                                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                                .collect();
                            let name = String::from_utf16_lossy(&chars);
                            if ns != 2 {
                                if fn_name.is_none() {
                                    fn_name = Some(name);
                                    fn_parent = pref & FRN_MASK;
                                    fn_mtime = pmod;
                                }
                            } else if dos_name.is_none() {
                                dos_name = Some(name);
                            }
                        }
                    }
                }
            }
            (0x80, _) if aname_len == 0 => {
                // $DATA（未命名流）→ 文件真实大小
                if nonres == 1 {
                    data_size = u64_at(rec, a + 48).unwrap_or(0);
                } else if let Some(vl) = u32_at(rec, a + 16) {
                    data_size = vl as u64;
                }
            }
            _ => {}
        }
        a += len;
    }

    let name = fn_name.or(dos_name).filter(|n| !n.is_empty())?;
    let mtime = if si_mtime != 0 { si_mtime } else { fn_mtime };
    let size = if is_dir { 0 } else { data_size };
    Some(MftRec { mft_no, parent_frn: fn_parent, name, size, mtime, is_dir })
}

// ============================ 卷上下文 ============================

pub struct MftCtx {
    pub h: win::HANDLE,
    pub letter: char,
    pub runs: Vec<(u64, u64, u64)>,
    pub record_size: usize,
    pub cluster: u32,
    pub mft_len: u64,
}

impl Drop for MftCtx {
    fn drop(&mut self) {
        if !self.h.is_null() {
            win::close_handle(self.h);
        }
    }
}

impl MftCtx {
    pub fn open(letter: char) -> Result<MftCtx, String> {
        let h = win::open_volume(letter).ok_or_else(|| format!("{letter}: 无法打开卷（需要管理员权限）"))?;
        let vd = win::query_ntfs_volume_data(h).ok_or_else(|| format!("{letter}: 读取卷数据失败"))?;
        let runs = win::mft_runs(h, vd.bytes_per_cluster, vd.mft_valid_data_length as u64)
            .ok_or_else(|| format!("{letter}: 读取 $MFT 布局失败"))?;
        if runs.is_empty() {
            return Err(format!("{letter}: $MFT run 列表为空"));
        }
        Ok(MftCtx {
            h,
            letter,
            runs,
            record_size: vd.bytes_per_file_record as usize,
            cluster: vd.bytes_per_cluster,
            mft_len: vd.mft_valid_data_length as u64,
        })
    }

    /// 读取单条 MFT 记录（USN 增量刷新用）。
    pub fn refresh_record(&mut self, frn: u64) -> Option<MftRec> {
        let no = frn & FRN_MASK;
        let logical = no.checked_mul(self.record_size as u64)?;
        if logical >= self.mft_len || self.record_size == 0 {
            return None;
        }
        // 定位 run；若超出覆盖范围（$MFT 扩展过），重取一次布局
        let run = self.find_run(logical).or_else(|| {
            if let Ok(newctx) = MftCtx::open(self.letter) {
                self.runs = newctx.runs.clone();
                self.mft_len = newctx.mft_len;
                self.find_run(logical)
            } else {
                None
            }
        })?;
        if logical >= self.mft_len {
            return None;
        }
        let phys = run.1 + (logical - run.0);
        let mut buf = vec![0u8; self.record_size];
        let mut moved: i64 = 0;
        let ok = unsafe { win::SetFilePointerEx(self.h, phys as i64, &mut moved, win::FILE_BEGIN) };
        if ok == 0 {
            return None;
        }
        let mut got: u32 = 0;
        let ok = unsafe { win::ReadFile(self.h, buf.as_mut_ptr() as _, buf.len() as u32, &mut got, std::ptr::null_mut()) };
        if ok == 0 || got as usize != self.record_size {
            return None;
        }
        parse_mft_record(&mut buf, no)
    }

    fn find_run(&self, logical: u64) -> Option<(u64, u64, u64)> {
        // runs 按 log_off 升序
        let mut lo = 0usize;
        let mut hi = self.runs.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.runs[mid].0 <= logical {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return None;
        }
        let r = self.runs[lo - 1];
        if logical < r.0 + r.2 {
            Some(r)
        } else {
            None
        }
    }
}

// ============================ MFT 全量扫描 ============================

fn rec_to_entry(r: &MftRec, letter: char) -> EntryRecord {
    let is_root = r.mft_no == MFT_ROOT;
    EntryRecord {
        frn: if is_root { MFT_ROOT } else { r.mft_no },
        parent_frn: if is_root { 0 } else { r.parent_frn },
        parent_idx: NO_PARENT,
        name: if is_root { format!("{letter}:") } else { r.name.clone() },
        size: r.size,
        mtime: r.mtime,
        flags: if r.is_dir { FLAG_DIR } else { 0 },
        is_root,
    }
}

/// 扫描一个 NTFS 卷。返回 (卷信息, 条目数)。
pub fn scan_volume_mft(
    letter: char,
    index: &Arc<RwLock<Index>>,
    status: &StatusHandle,
) -> Result<(crate::model::VolumeInfo, usize), String> {
    let mut ctx = MftCtx::open(letter)?;
    let (jid, nusn) = win::query_usn_journal(ctx.h)
        .map(|j| (j.journal_id, j.next_usn as u64))
        .unwrap_or((0, 0));
    let record_size = ctx.record_size;
    let mft_len = ctx.mft_len;
    let rs = record_size;
    let ml = mft_len;

    let idx2 = index.clone();
    let st2 = status.clone();
    let count = Arc::new(AtomicUsize::new(0));
    let count2 = count.clone();
    let mut pending: Vec<EntryRecord> = Vec::with_capacity(65_536);
    let mut last_ui = Instant::now();

    let result = win::read_mft_records(ctx.h, &ctx.runs, rs, ml, move |start_no, bytes| {
        let recs: Vec<Option<MftRec>> = bytes
            .par_chunks_mut(rs)
            .enumerate()
            .map(|(i, rec)| parse_mft_record(rec, start_no + i as u64))
            .collect();
        let mut ix = idx2.write().unwrap();
        for r in recs.into_iter().flatten() {
            if r.mft_no == MFT_ROOT {
                let rec = rec_to_entry(&r, letter);
                ix.add_entry(rec);
                count2.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if r.name.is_empty() {
                continue;
            }
            pending.push(rec_to_entry(&r, letter));
            if pending.len() >= 65_536 {
                flush_pending(&mut ix, &mut pending, &count2);
            }
        }
        flush_pending(&mut ix, &mut pending, &count2);
        if last_ui.elapsed() > Duration::from_millis(250) {
            last_ui = Instant::now();
            let c = count2.load(Ordering::Relaxed);
            st2.set(|s| s.phase = format!("正在索引 {letter}: · {c} 条"));
        }
    });

    result.map_err(|e| format!("{letter}: 读取 $MFT 失败: {e}"))?;

    Ok((
        crate::model::VolumeInfo { letter, serial: 0, kind: crate::model::VolumeKind::Mft, journal_id: jid, next_usn: nusn },
        count.load(Ordering::Relaxed),
    ))
}

fn flush_pending(ix: &mut Index, pending: &mut Vec<EntryRecord>, count: &AtomicUsize) {
    if pending.is_empty() {
        return;
    }
    let n = pending.len();
    for mut r in pending.drain(..) {
        r.parent_idx = if r.is_root {
            NO_PARENT
        } else {
            ix.frn_map.get(&r.parent_frn).copied().unwrap_or(NO_PARENT)
        };
        ix.add_entry(r);
    }
    count.fetch_add(n, Ordering::Relaxed);
}

// ============================ 目录遍历回退 ============================

fn st_to_filetime(t: Option<SystemTime>) -> u64 {
    match t {
        None => 0,
        Some(t) => {
            let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
            (d.as_secs() + 11_644_473_600) * 10_000_000 + (d.subsec_nanos() / 100) as u64
        }
    }
}

struct WalkState {
    q: VecDeque<(PathBuf, u32)>,
    pending: usize,
    done: bool,
}

pub fn scan_volume_walk(
    letter: char,
    index: &Arc<RwLock<Index>>,
    status: &StatusHandle,
    excluded: &[String],
) -> (crate::model::VolumeInfo, usize) {
    let excl: Vec<String> = excluded.iter().map(|p| crate::model::normalize_excl(p)).collect();
    let root_path = PathBuf::from(format!("{letter}:\\"));
    let root_idx = {
        let mut ix = index.write().unwrap();
        ix.add_entry(EntryRecord {
            frn: 0,
            parent_frn: 0,
            parent_idx: NO_PARENT,
            name: format!("{letter}:"),
            size: 0,
            mtime: 0,
            flags: FLAG_DIR,
            is_root: true,
        })
    };
    let n = walk_tree(root_path, root_idx, index, status, &excl, &format!("{letter}:"));

    (
        crate::model::VolumeInfo { letter, serial: 0, kind: crate::model::VolumeKind::Walk, journal_id: 0, next_usn: 0 },
        n,
    )
}

/// 遍历内核：从 `root_path` 出发，`root_idx` 是根在索引中的下标。
/// 与盘符解耦，便于对任意目录树复用（也便于验证父子链是否正确）。
fn walk_tree(
    root_path: PathBuf,
    root_idx: u32,
    index: &Arc<RwLock<Index>>,
    status: &StatusHandle,
    excl: &[String],
    label: &str,
) -> usize {
    let state = Arc::new((Mutex::new(WalkState { q: VecDeque::new(), pending: 0, done: false }), Condvar::new()));
    {
        let (m, _) = &*state;
        let mut g = m.lock().unwrap();
        g.q.push_back((root_path, root_idx));
        g.pending = 1;
    }
    let count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    let st2 = status.clone();
    let label = Arc::new(label.to_owned());
    for _ in 0..workers {
        let state = state.clone();
        let index = index.clone();
        let count = count.clone();
        let st = st2.clone();
        let excl = excl.to_vec();
        let label = label.clone();
        handles.push(std::thread::spawn(move || {
            // 文件与子目录分开攒：子目录必须先落盘拿到自己的下标，才能作为
            // 后续任务的 parent_idx 入队。混在一个批次里会导致「子目录内容挂到祖父」
            // —— 每层上跳一级，最终全部塌缩到卷根（路径列只显示 "G:"）。
            let mut files: Vec<EntryRecord> = Vec::with_capacity(512);
            let mut dirs: Vec<(PathBuf, EntryRecord)> = Vec::with_capacity(64);
            let mut last_ui = Instant::now();

            macro_rules! flush {
                () => {{
                    if !files.is_empty() || !dirs.is_empty() {
                        let mut queued: Vec<(PathBuf, u32)> = Vec::with_capacity(dirs.len());
                        {
                            let mut ix = index.write().unwrap();
                            let mut n = 0usize;
                            for r in files.drain(..) {
                                ix.add_entry(r);
                                n += 1;
                            }
                            for (sub, rec) in dirs.drain(..) {
                                let idx = ix.add_entry(rec);
                                queued.push((sub, idx));
                                n += 1;
                            }
                            count.fetch_add(n, Ordering::Relaxed);
                        }
                        // 入队必须在 pending -= 1 之前完成，否则本轮处理完毕后
                        // pending 会瞬间归零，其他 worker 提前退出。
                        if !queued.is_empty() {
                            let (m, cv) = &*state;
                            let mut g = m.lock().unwrap();
                            for it in queued {
                                g.q.push_back(it);
                                g.pending += 1;
                            }
                            drop(g);
                            cv.notify_all();
                        }
                    }
                }};
            }

            loop {
                // 取任务
                let task = {
                    let (m, cv) = &*state;
                    let mut g = m.lock().unwrap();
                    loop {
                        if let Some(t) = g.q.pop_front() {
                            break t;
                        }
                        if g.pending == 0 {
                            g.done = true;
                            cv.notify_all();
                            return;
                        }
                        g = cv.wait(g).unwrap();
                    }
                };
                let (dir, parent_idx) = task;
                // 枚举
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for entry in rd.flatten() {
                        let Ok(ft) = entry.file_type() else { continue };
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if ft.is_dir() {
                            let sub = dir.join(&name);
                            let sl = sub.to_string_lossy().to_lowercase();
                            if excl.iter().any(|x| crate::model::is_under(&sl, x)) {
                                continue;
                            }
                            if ft.is_symlink() {
                                continue; // 避免联接点死循环
                            }
                            dirs.push((
                                sub,
                                EntryRecord {
                                    frn: 0,
                                    parent_frn: 0,
                                    parent_idx,
                                    name,
                                    size: 0,
                                    mtime: 0,
                                    flags: FLAG_DIR,
                                    is_root: false,
                                },
                            ));
                        } else {
                            let (size, mtime) = entry
                                .metadata()
                                .map(|md| (md.len(), st_to_filetime(md.modified().ok())))
                                .unwrap_or((0, 0));
                            files.push(EntryRecord {
                                frn: 0,
                                parent_frn: 0,
                                parent_idx,
                                name,
                                size,
                                mtime,
                                flags: 0,
                                is_root: false,
                            });
                        }
                        if files.len() >= 1024 || dirs.len() >= 128 {
                            flush!();
                        }
                    }
                }
                flush!();

                // 进度
                let c = count.load(Ordering::Relaxed);
                if last_ui.elapsed() > Duration::from_millis(250) {
                    last_ui = Instant::now();
                    st.set(|s| s.phase = format!("正在遍历 {label} · {c} 条"));
                }

                // 任务完成
                let (m, cv) = &*state;
                let mut g = m.lock().unwrap();
                g.pending -= 1;
                if g.pending == 0 && g.q.is_empty() {
                    g.done = true;
                    cv.notify_all();
                    return;
                }
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    count.load(Ordering::Relaxed)
}

// ============================ USN 实时轮询 ============================

/// 目录属性位（winnt.h FILE_ATTRIBUTE_DIRECTORY）
const FILE_ATTR_DIRECTORY: u32 = 0x10;

enum UsnOp {
    /// 直接用 USN 记录自带的字段构造条目：不回读 MFT。
    Upsert { frn: u64, parent_frn: u64, name: String, is_dir: bool, mtime: u64 },
    Remove { frn: u64 },
}

fn decode_utf16(bytes: &[u8]) -> String {
    let mut v = Vec::with_capacity(bytes.len() / 2);
    for c in bytes.chunks_exact(2) {
        v.push(u16::from_le_bytes([c[0], c[1]]));
    }
    String::from_utf16_lossy(&v)
}

fn apply_usn_op(ix: &mut Index, op: UsnOp) {
    match op {
        UsnOp::Remove { frn } => {
            ix.remove_by_frn(frn);
        }
        UsnOp::Upsert { frn, parent_frn, name, is_dir, mtime } => {
            let existing = ix.frn_map.get(&frn).copied();
            let mut parent_idx = if parent_frn == 0 {
                NO_PARENT
            } else {
                ix.frn_map.get(&parent_frn).copied().unwrap_or(NO_PARENT)
            };
            // 父目录尚未入索引时保留原有父链，避免把已有路径打散
            if parent_idx == NO_PARENT {
                if let Some(i) = existing {
                    parent_idx = ix.entries[i as usize].parent_idx;
                }
            }
            let (size, old_mtime) = match existing {
                Some(i) => (ix.entries[i as usize].size, ix.entries[i as usize].mtime),
                None => (0, 0),
            };
            ix.upsert_record(EntryRecord {
                frn,
                parent_frn,
                parent_idx,
                name,
                size,
                mtime: if mtime != 0 { mtime } else { old_mtime },
                flags: if is_dir { FLAG_DIR } else { 0 },
                is_root: false,
            });
        }
    }
}

pub fn spawn_usn_poller(
    letter: char,
    journal_id: u64,
    from_usn: u64,
    index: Arc<RwLock<Index>>,
    status: StatusHandle,
    cancel: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name(format!("usn-{letter}"))
        .spawn(move || {
            let mut vol = match MftCtx::open(letter) {
                Ok(v) => v,
                Err(e) => {
                    status.set(|s| s.error = Some(format!("USN 监听失败: {e}")));
                    return;
                }
            };
            // 校验日志一致性
            match win::query_usn_journal(vol.h) {
                Some(j) if j.journal_id == journal_id => {}
                _ => {
                    status.set(|s| s.needs_rescan = true);
                    return;
                }
            }
            let mut next = from_usn as i64;
            let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
            let mut idle: u32 = 0;
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let Some((next2, payload)) = win::read_usn_records(vol.h, journal_id, next, &mut buf) else {
                    std::thread::sleep(Duration::from_millis(600));
                    continue;
                };
                next = next2;
                if payload == 0 {
                    // 空闲退避：400ms → 1.6s，避免无意义的空转
                    idle = (idle + 1).min(8);
                    std::thread::sleep(Duration::from_millis(400 + idle as u64 * 150));
                    continue;
                }
                idle = 0;

                let mut ops: Vec<UsnOp> = Vec::new();
                let mut refresh: Vec<u64> = Vec::new();
                let mut off = 8usize;
                let end = 8 + payload;
                while off + 60 <= end {
                    let Some(len) = u32_at(&buf, off) else { break };
                    let len = len as usize;
                    if len < 60 || off + len > end {
                        break;
                    }
                    let rec = off;
                    off += len;
                    if buf.get(rec + 4..rec + 6) != Some(&[2, 0]) {
                        continue; // 只处理 USN_RECORD_V2（64 位 FRN）
                    }
                    let frn = u64_at(&buf, rec + 8).unwrap_or(0);
                    let parent = u64_at(&buf, rec + 16).unwrap_or(0);
                    let ts = u64_at(&buf, rec + 32).unwrap_or(0);
                    let reason = u32_at(&buf, rec + 40).unwrap_or(0);
                    let attrs = u32_at(&buf, rec + 52).unwrap_or(0);
                    let name_bytes = u16_at(&buf, rec + 56).unwrap_or(0) as usize;
                    let name_off = u16_at(&buf, rec + 58).unwrap_or(0) as usize;
                    if frn == 0 {
                        continue;
                    }
                    if reason & win::USN_REASON_FILE_DELETE != 0 {
                        ops.push(UsnOp::Remove { frn });
                        continue;
                    }
                    // 新建 / 重命名：USN 记录里已有名字与父引用，直接落库
                    if reason & (win::USN_REASON_FILE_CREATE | win::USN_REASON_RENAME_NEW_NAME) != 0
                        && name_bytes >= 2
                    {
                        let s = rec + name_off;
                        if s + name_bytes <= buf.len() {
                            let name = decode_utf16(&buf[s..s + name_bytes]);
                            if !name.is_empty() {
                                ops.push(UsnOp::Upsert {
                                    frn,
                                    parent_frn: parent & FRN_MASK,
                                    name,
                                    is_dir: attrs & FILE_ATTR_DIRECTORY != 0,
                                    mtime: ts,
                                });
                                continue;
                            }
                        }
                    }
                    // 只有真正改变大小/属性的才需要回读 MFT
                    if reason
                        & (win::USN_REASON_DATA_OVERWRITE
                            | win::USN_REASON_DATA_EXTEND
                            | win::USN_REASON_DATA_TRUNCATION
                            | win::USN_REASON_BASIC_INFO_CHANGE)
                        != 0
                    {
                        refresh.push(frn);
                    }
                }

                let mut dirty = false;
                if !ops.is_empty() {
                    let mut ix = index.write().unwrap();
                    for op in ops {
                        apply_usn_op(&mut ix, op);
                    }
                    dirty = true;
                }
                if !refresh.is_empty() {
                    // 按 FRN 升序聚簇：MFT 内 FRN 近似物理有序，顺序回读远优于随机
                    refresh.sort_unstable();
                    refresh.dedup();
                    let mut recs = Vec::with_capacity(refresh.len());
                    for frn in refresh {
                        if let Some(r) = vol.refresh_record(frn) {
                            recs.push(r);
                        }
                    }
                    if !recs.is_empty() {
                        let mut ix = index.write().unwrap();
                        for r in recs {
                            let parent_idx = ix.frn_map.get(&r.parent_frn).copied().unwrap_or(NO_PARENT);
                            ix.upsert_record(EntryRecord {
                                frn: r.mft_no,
                                parent_frn: r.parent_frn,
                                parent_idx,
                                name: r.name,
                                size: r.size,
                                mtime: r.mtime,
                                flags: if r.is_dir { FLAG_DIR } else { 0 },
                                is_root: false,
                            });
                        }
                        dirty = true;
                    }
                }
                if dirty {
                    status.ctx.request_repaint();
                }
            }
        })
        .ok();
}

// ============================ 总协调 ============================

pub fn startup(excluded: Vec<String>, ctx: egui::Context) -> IndexHandle {
    let index = Arc::new(RwLock::new(Index::default()));
    let status = StatusHandle {
        st: Arc::new(Mutex::new(Status {
            phase: "准备中…".into(),
            ready: false,
            volumes_done: 0,
            volumes_total: 0,
            error: None,
            needs_rescan: false,
            mft_mode: false,
            started: Instant::now(),
        })),
        ctx: ctx.clone(),
    };
    let (excl_tx, excl_rx) = std::sync::mpsc::channel::<Vec<String>>();
    let (refresh_tx, refresh_rx) = std::sync::mpsc::channel::<()>();

    {
        let index = index.clone();
        let status = status.clone();
        std::thread::Builder::new()
            .name("index-coordinator".into())
            .spawn(move || coordinator(index, status, excl_rx, refresh_rx, excluded, ctx))
            .ok();
    }

    IndexHandle { index, status, excl_tx, refresh_tx }
}

fn plan_volumes() -> Vec<(char, u64, crate::model::VolumeKind)> {
    let mut plan = Vec::new();
    for letter in win::fixed_drives() {
        if let Some((serial, fs)) = win::volume_info(letter) {
            let kind = if fs.eq_ignore_ascii_case("ntfs") && win::is_admin() {
                crate::model::VolumeKind::Mft
            } else if fs.eq_ignore_ascii_case("ntfs") {
                crate::model::VolumeKind::Mft // 尝试 MFT，打开失败时回退遍历
            } else {
                crate::model::VolumeKind::Walk
            };
            plan.push((letter, serial, kind));
        }
    }
    plan
}

fn snapshot_matches(snap: &Index, plan: &[(char, u64, crate::model::VolumeKind)]) -> bool {
    if snap.volumes.len() != plan.len() {
        return false;
    }
    for (v, (letter, serial, kind)) in snap.volumes.iter().zip(plan) {
        if v.letter != *letter || v.serial != *serial || v.kind != *kind {
            return false;
        }
    }
    true
}

fn coordinator(
    index: Arc<RwLock<Index>>,
    status: StatusHandle,
    excl_rx: std::sync::mpsc::Receiver<Vec<String>>,
    refresh_rx: std::sync::mpsc::Receiver<()>,
    excluded: Vec<String>,
    ctx: egui::Context,
) {
    let plan = plan_volumes();
    let current_excl = Arc::new(Mutex::new(excluded.clone()));

    // 尝试快照恢复
    if let Ok(bytes) = std::fs::read(config::index_path()) {
        if let Some(snap) = load_snapshot(&bytes) {
            if snapshot_matches(&snap, &plan) {
                let total = snap.total();
                let vols = snap.volumes.clone();
                {
                    let mut ix = index.write().unwrap();
                    *ix = snap;
                    ix.recompute_exclusions(&excluded);
                }
                status.set(|s| {
                    s.ready = true;
                    s.phase = format!("已从缓存恢复 {total} 条");
                    s.mft_mode = vols.iter().any(|v| v.kind == crate::model::VolumeKind::Mft);
                });
                // 启动 USN 监听（从快照的 next_usn 回放）
                let cancel = Arc::new(AtomicBool::new(false));
                for v in vols.iter().filter(|v| v.kind == crate::model::VolumeKind::Mft && v.next_usn > 0) {
                    spawn_usn_poller(v.letter, v.journal_id, v.next_usn, index.clone(), status.clone(), cancel.clone());
                }
                coord_loop(index, status, excl_rx, refresh_rx, current_excl, ctx, cancel);
                return;
            }
        }
    }

    // 全量扫描
    full_scan(&index, &status, &plan, &excluded, &current_excl);
    coord_loop(index, status, excl_rx, refresh_rx, current_excl, ctx, Arc::new(AtomicBool::new(false)));
}

fn full_scan(
    index: &Arc<RwLock<Index>>,
    status: &StatusHandle,
    plan: &[(char, u64, crate::model::VolumeKind)],
    excluded: &[String],
    current_excl: &Arc<Mutex<Vec<String>>>,
) {
    status.set(|s| {
        s.ready = false;
        s.error = None;
        s.volumes_total = plan.len();
        s.volumes_done = 0;
        s.started = Instant::now();
        s.phase = "正在准备索引…".into();
    });
    {
        let mut ix = index.write().unwrap();
        *ix = Index::default();
    }

    let mut handles = Vec::new();
    for &(letter, serial, kind) in plan {
        let index = index.clone();
        let status = status.clone();
        let excl = excluded.to_vec();
        handles.push(std::thread::spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let start = Instant::now();
                // 非 NTFS 卷没有 MFT，直接走遍历（同样需要授权）
                if kind == crate::model::VolumeKind::Walk {
                    status.set(|s| s.volumes_done += 1);
                    if !ALLOW_WALK.load(Ordering::Relaxed) {
                        return Err(format!("{letter}: 非 NTFS 卷，需要开启遍历模式"));
                    }
                    let (mut vi, n) = scan_volume_walk(letter, &index, &status, &excl);
                    vi.serial = serial;
                    return Ok((vi, n));
                }
                let _ = start;
                match scan_volume_mft(letter, &index, &status) {
                    Ok((mut vi, n)) => {
                        vi.serial = serial;
                        status.set(|s| s.volumes_done += 1);
                        let _ = start.elapsed();
                        Ok((vi, n))
                    }
                    Err(e) => {
                        if ALLOW_WALK.load(Ordering::Relaxed) {
                            // 用户显式授权后才走遍历
                            let (mut vi, n) = scan_volume_walk(letter, &index, &status, &excl);
                            vi.serial = serial;
                            status.set(|s| s.volumes_done += 1);
                            Ok((vi, n))
                        } else {
                            status.set(|s| {
                                s.volumes_done += 1;
                                s.error = Some(format!(
                                    "{letter}: 需要管理员权限才能直读 MFT；已跳过（可在下方选择遍历模式）"
                                ));
                            });
                            Err(e)
                        }
                    }
                }
            }));
            res.unwrap_or_else(|_| {
                status.set(|s| s.volumes_done += 1);
                Err(format!("{letter}: 索引线程异常"))
            })
        }));
    }

    let mut vols = Vec::new();
    for h in handles {
        if let Ok(Ok((vi, _n))) = h.join() {
            vols.push(vi);
        }
    }
    vols.sort_by_key(|v| v.letter);

    let mft = vols.iter().any(|v| v.kind == crate::model::VolumeKind::Mft);
    {
        let mut ix = index.write().unwrap();
        ix.volumes = vols;
        let excl_now = current_excl.lock().unwrap().clone();
        ix.recompute_exclusions(&excl_now);
        let total = ix.total();
        drop(ix);
        let elapsed = status.get().started.elapsed().as_secs_f32();
        status.set(|s| {
            s.ready = true;
            s.mft_mode = mft;
            s.phase = if mft {
                format!("索引完成 · 用时 {elapsed:.1}s")
            } else {
                format!("索引完成（遍历模式）· 用时 {:.0}s", elapsed)
            };
            let _ = total;
        });
    }

    // 保存快照（含 next_usn 以便下次热启动）
    save_index_snapshot(index);

    // 启动 USN 监听
    let vols: Vec<crate::model::VolumeInfo> = index.read().unwrap().volumes.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    for v in vols.iter().filter(|v| v.kind == crate::model::VolumeKind::Mft && v.journal_id != 0) {
        spawn_usn_poller(v.letter, v.journal_id, v.next_usn, index.clone(), status.clone(), cancel.clone());
    }
    // 记录 cancel token 供重扫用
    *CANCEL_TOKEN.lock().unwrap() = Some(cancel);
}

static CANCEL_TOKEN: Mutex<Option<Arc<AtomicBool>>> = Mutex::new(None);

/// 是否允许在 MFT 不可用时回退到全盘目录遍历。
/// 默认关闭：遍历会产生 10^5~10^6 次随机元数据读，是最伤盘、最拖慢整机的路径，
/// 必须由用户显式开启。
static ALLOW_WALK: AtomicBool = AtomicBool::new(false);

pub fn set_allow_walk(v: bool) {
    ALLOW_WALK.store(v, Ordering::Relaxed);
}

pub fn allow_walk() -> bool {
    ALLOW_WALK.load(Ordering::Relaxed)
}

fn save_index_snapshot(index: &Arc<RwLock<Index>>) {
    let bytes = {
        let ix = index.read().unwrap();
        save_snapshot(&ix)
    };
    let path = config::index_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("bin.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn coord_loop(
    index: Arc<RwLock<Index>>,
    status: StatusHandle,
    excl_rx: std::sync::mpsc::Receiver<Vec<String>>,
    refresh_rx: std::sync::mpsc::Receiver<()>,
    current_excl: Arc<Mutex<Vec<String>>>,
    ctx: egui::Context,
    _cancel: Arc<AtomicBool>,
) {
    let mut last_epoch_saved: u64 = index.read().unwrap().epoch;
    let mut last_save = Instant::now();
    loop {
        match excl_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(new_excl) => {
                *current_excl.lock().unwrap() = new_excl.clone();
                {
                    let mut ix = index.write().unwrap();
                    ix.recompute_exclusions(&new_excl);
                }
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // 手动刷新
        while refresh_rx.try_recv().is_ok() {
            let plan = plan_volumes();
            let excl = current_excl.lock().unwrap().clone();
            full_scan(&index, &status, &plan, &excl, &current_excl);
            last_epoch_saved = index.read().unwrap().epoch;
        }
        // USN 检测到日志失效 → 全量重建
        if status.get().needs_rescan {
            status.set(|s| {
                s.needs_rescan = false;
                s.phase = "USN 日志失效，正在重建索引…".into();
            });
            if let Some(tok) = CANCEL_TOKEN.lock().unwrap().take() {
                tok.store(true, Ordering::Relaxed);
            }
            let plan = plan_volumes();
            let excl = current_excl.lock().unwrap().clone();
            full_scan(&index, &status, &plan, &excl, &current_excl);
            last_epoch_saved = index.read().unwrap().epoch;
        }
        // 周期性快照
        if last_save.elapsed() > Duration::from_secs(120) {
            last_save = Instant::now();
            let cur = index.read().unwrap().epoch;
            if cur != last_epoch_saved {
                last_epoch_saved = cur;
                save_index_snapshot(&index);
            }
        }
    }
}

// 便于外部查询 admin 状态
pub fn admin_available() -> bool {
    win::is_admin()
}

/// 手动重建（由 UI 调用）
pub fn request_refresh(handle: &IndexHandle) {
    let _ = handle.refresh_tx.send(());
}

#[cfg(test)]
mod walk_probe {
    use super::*;

    /// 遍历必须建立正确的父链：修复前每层都上跳一级（子目录内容挂到祖父），
    /// 所有条目最终塌缩到卷根，路径列只显示 "G:"。
    #[test]
    fn walk_keeps_full_parent_chain() {
        let base = std::env::temp_dir().join("xunsou_walk_probe");
        let _ = std::fs::remove_dir_all(&base);
        let deep = base.join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("leaf.txt"), b"x").unwrap();
        std::fs::write(base.join("a").join("mid.png"), b"y").unwrap();

        let ctx = egui::Context::default();
        let status = StatusHandle::new(
            ctx,
            Status {
                phase: String::new(),
                ready: false,
                volumes_done: 0,
                volumes_total: 1,
                error: None,
                needs_rescan: false,
                mft_mode: false,
                started: Instant::now(),
            },
        );
        let index = Arc::new(RwLock::new(Index::default()));
        let root_idx = {
            let mut ix = index.write().unwrap();
            ix.add_entry(EntryRecord {
                frn: 0,
                parent_frn: 0,
                parent_idx: NO_PARENT,
                name: "T:".into(),
                size: 0,
                mtime: 0,
                flags: FLAG_DIR,
                is_root: true,
            })
        };
        let n = walk_tree(base.clone(), root_idx, &index, &status, &[], "T:");
        assert_eq!(n, 5, "应索引 3 层目录 + 2 个文件");

        let ix = index.read().unwrap();
        let mut got: Vec<String> = Vec::new();
        for i in 0..ix.entries.len() {
            let name = ix.name_of(&ix.entries[i]);
            if name == "leaf.txt" || name == "mid.png" {
                got.push(ix.path_of(i as u32));
            }
        }
        got.sort();
        assert!(
            got.iter().any(|p| p.ends_with(&format!("a{sep}b{sep}c{sep}leaf.txt", sep = '\\'))),
            "深层文件路径错误: {got:?}"
        );
        assert!(
            got.iter().any(|p| p.ends_with(&format!("a{sep}mid.png", sep = '\\'))),
            "二层文件路径错误: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

//! 索引数据模型：紧凑条目 + 去重名称池 + 父链路径重建 + 二进制快照。

use std::collections::{HashMap, HashSet};

pub const FLAG_DIR: u8 = 1;
pub const FLAG_EXCLUDED: u8 = 2;

pub const NO_PARENT: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub frn: u64,        // NTFS 文件引用号（目录遍历模式为 0）
    pub parent_frn: u64, // 父目录 frn（MFT 模式用于重链）
    pub parent_idx: u32, // 父条目下标；根为 NO_PARENT
    pub name_off: u32,   // 名称在名称池中的偏移（原名与小写池同偏移同长度）
    pub name_len: u16,
    pub size: u64,
    pub mtime: u64, // Windows FILETIME
    pub flags: u8,
}

impl Entry {
    #[inline]
    pub fn is_dir(&self) -> bool {
        self.flags & FLAG_DIR != 0
    }
    #[inline]
    pub fn is_excluded(&self) -> bool {
        self.flags & FLAG_EXCLUDED != 0
    }
}

#[derive(Clone, Debug)]
pub struct VolumeInfo {
    pub letter: char,
    pub serial: u64,
    pub kind: VolumeKind,
    pub journal_id: u64,
    pub next_usn: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VolumeKind {
    Mft,
    Walk,
}

/// 名称去重池：原名与小写各一份，但共享 (off,len)。
/// 仅做 ASCII 小写化（中文不受影响，极少数 Unicode 特例按原文匹配）。
#[derive(Default)]
pub struct NamePool {
    pub orig: Vec<u8>,
    pub lower: Vec<u8>,
    dedup_orig: HashMap<u64, Vec<u32>>, // hash -> offsets
    dedup_lower: HashMap<u64, Vec<u32>>,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

impl NamePool {
    pub fn intern(&mut self, name: &str) -> (u32, u16) {
        let bytes = name.as_bytes();
        let mut lower_bytes = bytes.to_vec();
        lower_bytes.make_ascii_lowercase();
        let h = fnv(&lower_bytes);
        if let Some(cands) = self.dedup_lower.get(&h) {
            for &off in cands {
                let off = off as usize;
                if self.lower[off..off + bytes.len()] == lower_bytes[..] {
                    return (off as u32, bytes.len() as u16);
                }
            }
        }
        let off = self.orig.len() as u32;
        self.orig.extend_from_slice(bytes);
        self.lower.extend_from_slice(&lower_bytes);
        self.dedup_orig
            .entry(fnv(bytes))
            .or_default()
            .push(off);
        self.dedup_lower.entry(h).or_default().push(off);
        (off, bytes.len() as u16)
    }

    #[inline]
    pub fn name(&self, off: u32, len: u16) -> &str {
        let a = off as usize;
        let b = a + len as usize;
        std::str::from_utf8(&self.orig[a..b]).unwrap_or("")
    }

    #[inline]
    pub fn lower_name(&self, off: u32, len: u16) -> &[u8] {
        let a = off as usize;
        let b = a + len as usize;
        &self.lower[a..b]
    }
}

/// 全局索引（多卷共享一个条目表）。
#[derive(Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    pub pool: NamePool,
    pub frn_map: HashMap<u64, u32>,
    pub volumes: Vec<VolumeInfo>,
    /// 等待父目录出现的孤儿条目下标
    pub orphans: HashSet<u32>,
    /// 父 FRN → 等待它的孤儿下标。把 O(目录数 × 孤儿数) 的扫描降为 O(1)。
    pub waiting: HashMap<u64, Vec<u32>>,
    pub epoch: u64,
}

impl Index {
    pub fn total(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn name_of(&self, e: &Entry) -> &str {
        self.pool.name(e.name_off, e.name_len)
    }

    #[inline]
    pub fn lower_of(&self, e: &Entry) -> &[u8] {
        self.pool.lower_name(e.name_off, e.name_len)
    }

    /// 添加条目（自动去重名称、维护 frn 映射与孤儿重链）。
    pub fn add_entry(&mut self, rec: EntryRecord) -> u32 {
        let (off, len) = self.pool.intern(&rec.name);
        let idx = self.entries.len() as u32;
        let e = Entry {
            frn: rec.frn,
            parent_frn: rec.parent_frn,
            parent_idx: rec.parent_idx,
            name_off: off,
            name_len: len,
            size: rec.size,
            mtime: rec.mtime,
            flags: rec.flags,
        };
        self.entries.push(e);
        if rec.frn != 0 {
            self.frn_map.insert(rec.frn, idx);
        }
        if rec.parent_idx == NO_PARENT && rec.parent_frn != 0 && !rec.is_root {
            self.orphans.insert(idx);
            self.waiting.entry(rec.parent_frn).or_default().push(idx);
        }
        // 新目录出现时，O(1) 取出等待它的孤儿并重链
        if e.is_dir() && rec.frn != 0 {
            if let Some(ws) = self.waiting.remove(&rec.frn) {
                for oi in ws {
                    if (oi as usize) < self.entries.len() {
                        self.entries[oi as usize].parent_idx = idx;
                    }
                    self.orphans.remove(&oi);
                    self.epoch += 1;
                }
            }
        }
        self.epoch += 1;
        idx
    }

    pub fn remove_by_frn(&mut self, frn: u64) -> bool {
        if let Some(idx) = self.frn_map.remove(&frn) {
            // 从条目表移除（标记为墓碑：与末尾交换后 pop）
            let last = self.entries.len() - 1;
            if idx as usize != last {
                self.entries.swap(idx as usize, last);
                // 修正被移动条目的 frn 映射
                let moved = self.entries[idx as usize];
                if moved.frn != 0 {
                    self.frn_map.insert(moved.frn, idx);
                }
            }
            self.entries.pop();
            self.orphans.remove(&idx);
            // 被移动到 idx 的条目可能仍在 waiting 表里，修正其下标
            if (idx as usize) < self.entries.len() {
                let moved = self.entries[idx as usize];
                if moved.parent_idx == NO_PARENT && moved.parent_frn != 0 {
                    if let Some(v) = self.waiting.get_mut(&moved.parent_frn) {
                        for x in v.iter_mut() {
                            if *x as usize == last {
                                *x = idx;
                            }
                        }
                    }
                }
            }
            self.epoch += 1;
            true
        } else {
            false
        }
    }

    /// USN 增量更新：存在则原地刷新，不存在则新增；继承父目录的排除状态。
    pub fn upsert_record(&mut self, rec: EntryRecord) {
        let frn = rec.frn;
        if let Some(&idx) = self.frn_map.get(&frn) {
            let parent_excluded = rec.parent_idx != NO_PARENT
                && (rec.parent_idx as usize) < self.entries.len()
                && self.entries[rec.parent_idx as usize].is_excluded();
            let was_excluded = self.entries[idx as usize].is_excluded();
            let (off, len) = self.pool.intern(&rec.name);
            {
                let e = &mut self.entries[idx as usize];
                e.name_off = off;
                e.name_len = len;
                e.size = rec.size;
                e.mtime = rec.mtime;
                e.flags = rec.flags | if was_excluded || parent_excluded { FLAG_EXCLUDED } else { 0 };
                e.parent_frn = rec.parent_frn;
                e.parent_idx = rec.parent_idx;
            }
            if rec.parent_idx == NO_PARENT && rec.parent_frn != 0 && !rec.is_root {
                if self.orphans.insert(idx) {
                    self.waiting.entry(rec.parent_frn).or_default().push(idx);
                }
            } else if self.orphans.remove(&idx) {
                if let Some(v) = self.waiting.get_mut(&self.entries[idx as usize].parent_frn) {
                    v.retain(|&x| x != idx);
                }
            }
            self.epoch += 1;
        } else {
            let is_root = rec.is_root;
            let idx = self.add_entry(rec);
            let parent_idx = self.entries[idx as usize].parent_idx;
            if !is_root
                && parent_idx != NO_PARENT
                && (parent_idx as usize) < self.entries.len()
                && self.entries[parent_idx as usize].is_excluded()
            {
                self.entries[idx as usize].flags |= FLAG_EXCLUDED;
            }
        }
    }

    /// 由父链重建完整路径（仅用于展示/少量行；根返回 "C:"）。
    pub fn path_of(&self, idx: u32) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(16);
        let mut cur = idx;
        let mut hops = 0usize;
        loop {
            if cur as usize >= self.entries.len() {
                break;
            }
            let e = &self.entries[cur as usize];
            parts.push(self.name_of(e));
            if e.parent_idx == NO_PARENT {
                break;
            }
            cur = e.parent_idx;
            hops += 1;
            if hops > 128 {
                break;
            }
        }
        parts.reverse();
        if parts.is_empty() {
            return String::new();
        }
        let mut s = String::with_capacity(64);
        s.push_str(parts[0]);
        for p in &parts[1..] {
            s.push('\\');
            s.push_str(p);
        }
        s
    }

    /// 父目录路径（不含自身名称）。根与孤儿返回空串。
    pub fn parent_path(&self, idx: u32) -> String {
        let Some(e) = self.entries.get(idx as usize) else {
            return String::new();
        };
        if e.parent_idx == NO_PARENT {
            return String::new();
        }
        self.path_of(e.parent_idx)
    }

    /// 重算排除标记：先把被排除目录打标，再传播到全部子孙。
    pub fn recompute_exclusions(&mut self, excluded: &[String]) {
        if excluded.is_empty() {
            let changed = self.entries.iter().any(|e| e.is_excluded());
            if changed {
                for e in self.entries.iter_mut() {
                    e.flags &= !FLAG_EXCLUDED;
                }
                self.epoch += 1;
            }
            return;
        }
        let excl: Vec<String> = excluded
            .iter()
            .map(|p| normalize_excl(p))
            .filter(|p| !p.is_empty())
            .collect();
        if excl.is_empty() {
            return;
        }
        // 1) 目录打标
        for i in 0..self.entries.len() {
            let is_dir = self.entries[i].is_dir();
            if !is_dir {
                continue;
            }
            let path = self.path_of(i as u32);
            let lower = path.to_lowercase();
            let hit = excl.iter().any(|x| is_under(&lower, x));
            self.entries[i].flags = if hit {
                self.entries[i].flags | FLAG_EXCLUDED
            } else {
                self.entries[i].flags & !FLAG_EXCLUDED
            };
        }
        // 2) 传播到子孙：沿父链找已打标祖先
        for i in 0..self.entries.len() {
            if self.entries[i].is_dir() {
                continue; // 目录已在步骤 1 处理
            }
            let mut cur = self.entries[i].parent_idx;
            let mut hops = 0;
            let mut excluded_hit = false;
            while cur != NO_PARENT && hops < 128 {
                if (cur as usize) >= self.entries.len() {
                    break;
                }
                if self.entries[cur as usize].is_excluded() {
                    excluded_hit = true;
                    break;
                }
                cur = self.entries[cur as usize].parent_idx;
                hops += 1;
            }
            self.entries[i].flags = if excluded_hit {
                self.entries[i].flags | FLAG_EXCLUDED
            } else {
                self.entries[i].flags & !FLAG_EXCLUDED
            };
        }
        self.epoch += 1;
    }
}

pub struct EntryRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub parent_idx: u32,
    pub name: String,
    pub size: u64,
    pub mtime: u64,
    pub flags: u8,
    pub is_root: bool,
}

pub fn normalize_excl(p: &str) -> String {
    let mut s = p.trim().to_lowercase();
    while s.ends_with('\\') && s.len() > 3 {
        s.pop();
    }
    if s == "c:" || (s.len() == 2 && s.as_bytes()[1] == b':') {
        s.push('\\');
    }
    s
}

/// path_lower 是否位于排除目录 excl 之内（含边界一致）。
pub fn is_under(path_lower: &str, excl: &str) -> bool {
    if excl.ends_with('\\') {
        // 盘根："c:\"
        path_lower.starts_with(excl) || path_lower == excl.trim_end_matches('\\')
    } else {
        path_lower == excl || path_lower.starts_with(&format!("{excl}\\"))
    }
}

// ---------- 快照序列化 ----------

/// 快照格式版本。 magic 里的两位数字随格式变更递增：
/// 旧版本快照会在加载时被静默丢弃，触发一次重建。
/// XUNSOU02：遍历路径曾把父链写错（子目录挂到祖父），旧快照里的 parent_idx 不可信。
pub const SNAPSHOT_MAGIC: &[u8; 8] = b"XUNSOU02";

pub fn save_snapshot(index: &Index) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(64 + index.entries.len() * 48 + index.pool.orig.len());
    out.extend_from_slice(SNAPSHOT_MAGIC);
    push_u32(&mut out, index.entries.len() as u32);
    push_u32(&mut out, index.pool.orig.len() as u32);
    out.extend_from_slice(&index.pool.orig);
    push_u32(&mut out, index.volumes.len() as u32);
    for v in &index.volumes {
        out.push(v.letter as u8);
        push_u64(&mut out, v.serial);
        out.push(match v.kind {
            VolumeKind::Mft => 1,
            VolumeKind::Walk => 2,
        });
        push_u64(&mut out, v.journal_id);
        push_u64(&mut out, v.next_usn);
    }
    for e in &index.entries {
        push_u64(&mut out, e.frn);
        push_u64(&mut out, e.parent_frn);
        push_u32(&mut out, e.parent_idx);
        push_u32(&mut out, e.name_off);
        push_u16(&mut out, e.name_len);
        push_u64(&mut out, e.size);
        push_u64(&mut out, e.mtime);
        out.push(e.flags);
    }
    out
}

pub fn load_snapshot(bytes: &[u8]) -> Option<Index> {
    if bytes.len() < 12 || &bytes[0..8] != SNAPSHOT_MAGIC {
        return None;
    }
    let mut p = 8usize;
    let n_entries = read_u32(bytes, &mut p)? as usize;
    let pool_len = read_u32(bytes, &mut p)? as usize;
    if p + pool_len > bytes.len() {
        return None;
    }
    let orig = bytes[p..p + pool_len].to_vec();
    p += pool_len;
    let mut pool = NamePool::default();
    pool.orig = orig.clone();
    // lower 池按条目引用的偏移稀疏重建（原名池可能含已删除条目的空洞）
    pool.lower = vec![0u8; pool.orig.len()];

    let n_vols = read_u32(bytes, &mut p)? as usize;
    let mut volumes = Vec::with_capacity(n_vols);
    for _ in 0..n_vols {
        if p >= bytes.len() {
            return None;
        }
        let letter = bytes[p] as char;
        p += 1;
        let serial = read_u64(bytes, &mut p)?;
        let kind = match bytes[p] {
            1 => VolumeKind::Mft,
            2 => VolumeKind::Walk,
            _ => return None,
        };
        p += 1;
        let journal_id = read_u64(bytes, &mut p)?;
        let next_usn = read_u64(bytes, &mut p)?;
        volumes.push(VolumeInfo { letter, serial, kind, journal_id, next_usn });
    }

    let mut entries = Vec::with_capacity(n_entries);
    for _ in 0..n_entries {
        let frn = read_u64(bytes, &mut p)?;
        let parent_frn = read_u64(bytes, &mut p)?;
        let parent_idx = read_u32(bytes, &mut p)?;
        let name_off = read_u32(bytes, &mut p)?;
        let name_len = read_u16(bytes, &mut p)?;
        let size = read_u64(bytes, &mut p)?;
        let mtime = read_u64(bytes, &mut p)?;
        let flags = *bytes.get(p)?;
        p += 1;
        entries.push(Entry { frn, parent_frn, parent_idx, name_off, name_len, size, mtime, flags });
    }

    // 按偏移重建 lower 池与去重表
    for e in &entries {
        let a = e.name_off as usize;
        let b = a + e.name_len as usize;
        if b > pool.orig.len() {
            return None;
        }
        let lowered: Vec<u8> = pool.orig[a..b].to_ascii_lowercase();
        pool.lower[a..b].copy_from_slice(&lowered);
        let _ = pool.dedup_orig.entry(fnv(&pool.orig[a..b])).or_default();
        let _ = pool.dedup_lower.entry(fnv(&lowered)).or_default();
    }
    // dedup 表不 push 偏移也没关系：仅用于快照后新名称的去重；旧偏移不参与。
    debug_assert_eq!(pool.lower.len(), pool.orig.len());

    let frn_map: HashMap<u64, u32> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.frn != 0)
        .map(|(i, e)| (e.frn, i as u32))
        .collect();
    let orphans: HashSet<u32> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.parent_idx == NO_PARENT && e.parent_frn != 0)
        .map(|(i, _)| i as u32)
        .collect();

    let waiting: HashMap<u64, Vec<u32>> = orphans
        .iter()
        .filter_map(|&i| {
            let e = &entries[i as usize];
            if e.parent_frn != 0 {
                Some((e.parent_frn, i))
            } else {
                None
            }
        })
        .fold(HashMap::new(), |mut m: HashMap<u64, Vec<u32>>, (k, i)| {
            m.entry(k).or_default().push(i);
            m
        });

    Some(Index { entries, pool, frn_map, volumes, orphans, waiting, epoch: 0 })
}

fn push_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn push_u64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}
fn read_u32(b: &[u8], p: &mut usize) -> Option<u32> {
    let s = b.get(*p..*p + 4)?;
    *p += 4;
    Some(u32::from_le_bytes(s.try_into().ok()?))
}
fn read_u16(b: &[u8], p: &mut usize) -> Option<u16> {
    let s = b.get(*p..*p + 2)?;
    *p += 2;
    Some(u16::from_le_bytes(s.try_into().ok()?))
}
fn read_u64(b: &[u8], p: &mut usize) -> Option<u64> {
    let s = b.get(*p..*p + 8)?;
    *p += 8;
    Some(u64::from_le_bytes(s.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, parent_idx: u32, parent_frn: u64, frn: u64, dir: bool) -> EntryRecord {
        EntryRecord {
            frn,
            parent_frn,
            parent_idx,
            name: name.into(),
            size: 123,
            mtime: 0,
            flags: if dir { FLAG_DIR } else { 0 },
            is_root: false,
        }
    }

    #[test]
    fn pool_dedups() {
        let mut p = NamePool::default();
        let (o1, l1) = p.intern("hello.txt");
        let (o2, l2) = p.intern("hello.txt");
        let (o3, _) = p.intern("WORLD.TXT");
        assert_eq!((o1, l1), (o2, l2));
        assert_ne!(o1, o3);
        assert_eq!(p.name(o3, 9), "WORLD.TXT");
        assert_eq!(p.lower_name(o3, 9), b"world.txt");
    }

    #[test]
    fn path_build_and_orphan_relink() {
        let mut ix = Index::default();
        let root = ix.add_entry(EntryRecord {
            frn: 5,
            parent_frn: 0,
            parent_idx: NO_PARENT,
            name: "C:".into(),
            size: 0,
            mtime: 0,
            flags: FLAG_DIR,
            is_root: true,
        });
        // 子目录先来（孤儿）
        let dir = ix.add_entry(rec("Users", NO_PARENT, 5, 100, true));
        assert_eq!(dir, 1);
        let file = ix.add_entry(rec("a.txt", dir, 100, 101, false));
        // 根重链后路径可用
        ix.entries[dir as usize].parent_idx = root;
        assert_eq!(ix.path_of(file), "C:\\Users\\a.txt");
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut ix = Index::default();
        let root = ix.add_entry(EntryRecord {
            frn: 5,
            parent_frn: 0,
            parent_idx: NO_PARENT,
            name: "D:".into(),
            size: 0,
            mtime: 0,
            flags: FLAG_DIR,
            is_root: true,
        });
        let d = ix.add_entry(rec("data", root, 5, 7, true));
        ix.add_entry(rec("x.mp4", d, 7, 8, false));
        let bytes = save_snapshot(&ix);
        let ix2 = load_snapshot(&bytes).unwrap();
        assert_eq!(ix2.total(), 3);
        assert_eq!(ix2.volumes.len(), ix.volumes.len());
        // 验证 lower 池与路径
        assert_eq!(ix2.lower_of(&ix2.entries[2]), b"x.mp4");
        assert_eq!(ix2.path_of(2), "D:\\data\\x.mp4");
    }

    #[test]
    fn exclusion_prefix() {
        let mut ix = Index::default();
        let root = ix.add_entry(EntryRecord {
            frn: 5,
            parent_frn: 0,
            parent_idx: NO_PARENT,
            name: "C:".into(),
            size: 0,
            mtime: 0,
            flags: FLAG_DIR,
            is_root: true,
        });
        let d = ix.add_entry(rec("Windows", root, 5, 10, true));
        let f = ix.add_entry(rec("notepad.exe", d, 10, 11, false));
        ix.recompute_exclusions(&["C:\\".to_string()]);
        assert!(ix.entries[d as usize].is_excluded());
        assert!(ix.entries[f as usize].is_excluded());
    }
}

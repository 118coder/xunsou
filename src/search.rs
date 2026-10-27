//! 并行搜索引擎：分词 + rayon 分块匹配 + 增量收窄。

use rayon::prelude::*;

use crate::categories::{classify, Category};
use crate::model::{Index, FLAG_DIR};

/// 搜索令牌：普通令牌匹配文件名；含 '\\' 的令牌按路径模式（各段需同时命中目录路径或文件名）。
#[derive(Clone, Debug)]
pub enum Token {
    Name(String),
    PathParts(Vec<String>),
}

pub fn parse_tokens(query: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for raw in query.split_whitespace() {
        if raw.contains('\\') {
            let parts: Vec<String> = raw
                .split(['\\', '/'])
                .filter(|s| !s.is_empty())
                .map(|s| s.to_lowercase())
                .collect();
            if parts.is_empty() {
                continue;
            }
            out.push(Token::PathParts(parts));
        } else {
            out.push(Token::Name(raw.to_lowercase()));
        }
    }
    out
}

#[inline]
fn cat_ok(cat: Option<Category>, is_dir: bool, name: &str) -> bool {
    match cat {
        None => true,
        Some(Category::All) => true,
        Some(c) => classify(name, is_dir) == Some(c),
    }
}

/// 单条目是否命中全部令牌（调用方保证下标合法）。
#[inline]
fn entry_matches(index: &Index, ei: u32, tokens: &[Token]) -> bool {
    let Some(e) = index.entries.get(ei as usize) else {
        return false;
    };
    let lower = index.lower_of(e);
    let mut dir_lower: Option<String> = None;
    for t in tokens {
        let ok = match t {
            Token::Name(s) => contains_bytes(lower, s.as_bytes()),
            Token::PathParts(parts) => {
                if dir_lower.is_none() {
                    dir_lower = Some(parent_path_lower(index, *e));
                }
                let d = dir_lower.as_deref().unwrap_or("");
                parts
                    .iter()
                    .all(|p| contains_bytes(lower, p.as_bytes()) || contains_bytes(d.as_bytes(), p.as_bytes()))
            }
        };
        if !ok {
            return false;
        }
    }
    true
}

/// 全量并行搜索，返回命中的条目下标（索引序）。
pub fn search(index: &Index, tokens: &[Token], cat: Option<Category>) -> Vec<u32> {
    if tokens.is_empty() {
        // 空查询：仅分类过滤
        let mut out: Vec<u32> = Vec::new();
        match cat {
            None | Some(Category::All) => {
                out.extend(0..index.entries.len() as u32);
            }
            Some(_) => {
                for (i, e) in index.entries.iter().enumerate() {
                    if e.is_excluded() {
                        continue;
                    }
                    let name = index.name_of(e);
                    if cat_ok(cat, e.is_dir(), name) {
                        out.push(i as u32);
                    }
                }
            }
        }
        return out;
    }

    let entries = &index.entries;
    let chunk = 262_144;
    entries
        .par_chunks(chunk)
        .enumerate()
        .flat_map_iter(move |(ci, ck)| {
            let base = ci * chunk;
            (0..ck.len()).filter_map(move |i| {
                let e = &ck[i];
                if e.is_excluded() {
                    return None;
                }
                if !cat_ok(cat, e.is_dir(), index.name_of(e)) {
                    return None;
                }
                let idx = (base + i) as u32;
                if entry_matches(index, idx, tokens) {
                    Some(idx)
                } else {
                    None
                }
            })
        })
        .collect()
}

/// 在已有结果集内做增量收窄：仅当新查询是旧查询的前缀追加时成立，
/// 此时「命中新查询」必然蕴含「命中旧查询」。成本 O(上次结果)。
pub fn search_within(index: &Index, prev: &[u32], tokens: &[Token], cat: Option<Category>) -> Vec<u32> {
    if tokens.is_empty() && matches!(cat, None | Some(Category::All)) {
        return prev.to_vec();
    }
    prev.par_iter()
        .copied()
        .filter(|&ei| {
            let Some(e) = index.entries.get(ei as usize) else {
                return false;
            };
            if e.is_excluded() {
                return false;
            }
            if !cat_ok(cat, e.is_dir(), index.name_of(e)) {
                return false;
            }
            tokens.is_empty() || entry_matches(index, ei, tokens)
        })
        .collect()
}

/// 分类计数（侧栏用）。一次遍历，零分配。
pub fn count_by_category(index: &Index) -> [u32; 7] {
    index
        .entries
        .par_iter()
        .fold(
            || [0u32; 7],
            |mut acc: [u32; 7], e| {
                if e.is_excluded() {
                    return acc;
                }
                acc[0] += 1;
                if let Some(c) = classify(index.name_of(e), e.is_dir()) {
                    acc[c.index()] += 1;
                }
                acc
            },
        )
        .reduce(
            || [0u32; 7],
            |a, b| {
                let mut r = a;
                for i in 0..7 {
                    r[i] += b[i];
                }
                r
            },
        )
}

/// 父目录的完整小写路径（沿 parent_idx 拼接）。
fn parent_path_lower(index: &Index, e: crate::model::Entry) -> String {
    let mut parts: Vec<&[u8]> = Vec::with_capacity(16);
    let mut cur = e.parent_idx;
    let mut hops = 0;
    loop {
        if cur == crate::model::NO_PARENT || cur as usize >= index.entries.len() || hops > 128 {
            break;
        }
        let pe = &index.entries[cur as usize];
        parts.push(index.lower_of(pe));
        cur = pe.parent_idx;
        hops += 1;
    }
    parts.reverse();
    let mut s = String::with_capacity(48);
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            s.push('\\');
        }
        s.push_str(&String::from_utf8_lossy(p));
    }
    s
}

#[inline]
fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    // 轻量 memmem（避免额外依赖）
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if hay[i] == first && &hay[i..i + needle.len()] == needle {
            return true;
        }
        i += 1;
    }
    false
}

/// 结果排序（原地，rayon 并行）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    None,
    Name,
    Path,
    Size,
    Mtime,
}

pub fn sort_results(index: &Index, results: &mut [u32], key: SortKey, desc: bool) {
    if key == SortKey::None || results.len() < 2 {
        return;
    }
    if key == SortKey::Path && results.len() > 300_000 {
        // 路径排序需要为每个结果构建完整路径，超大规模时退化为按名排序
        sort_results(index, results, SortKey::Name, desc);
        return;
    }
    match key {
        SortKey::Name => {
            results.par_sort_unstable_by(|&a, &b| {
                let ea = &index.entries[a as usize];
                let eb = &index.entries[b as usize];
                index
                    .lower_of(ea)
                    .cmp(index.lower_of(eb))
                    .then_with(|| ea.frn.cmp(&eb.frn))
            });
        }
        SortKey::Path => {
            let mut keyed: Vec<(String, u32)> = results
                .iter()
                .map(|&i| (index.path_of(i), i))
                .collect();
            keyed.par_sort_unstable_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            for (dst, (_, src)) in results.iter_mut().zip(keyed) {
                *dst = src;
            }
        }
        SortKey::Size => {
            results.par_sort_unstable_by(|&a, &b| {
                let ea = &index.entries[a as usize];
                let eb = &index.entries[b as usize];
                ea.size.cmp(&eb.size).then_with(|| ea.frn.cmp(&eb.frn))
            });
        }
        SortKey::Mtime => {
            results.par_sort_unstable_by(|&a, &b| {
                let ea = &index.entries[a as usize];
                let eb = &index.entries[b as usize];
                ea.mtime.cmp(&eb.mtime).then_with(|| ea.frn.cmp(&eb.frn))
            });
        }
        SortKey::None => {}
    }
    if desc {
        results.reverse();
    }
}

/// 目录判定便捷。
#[inline]
pub fn is_dir_flags(flags: u8) -> bool {
    flags & FLAG_DIR != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryRecord, NO_PARENT};

    fn mk(name: &str, frn: u64, parent_idx: u32, parent_frn: u64, dir: bool) -> EntryRecord {
        EntryRecord {
            frn,
            parent_frn,
            parent_idx,
            name: name.into(),
            size: 1,
            mtime: 1,
            flags: if dir { FLAG_DIR } else { 0 },
            is_root: false,
        }
    }

    #[test]
    fn search_and_match() {
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
        let d = ix.add_entry(mk("Photos", 10, root, 5, true));
        let f1 = ix.add_entry(mk("SUNSET.jpg", 11, d, 10, false));
        let f2 = ix.add_entry(mk("readme.TXT", 12, root, 5, false));
        ix.recompute_exclusions(&[]);

        let toks = parse_tokens("sunset JPG");
        let r = search(&ix, &toks, None);
        assert_eq!(r, vec![f1]);

        let toks = parse_tokens("readme");
        let r = search(&ix, &toks, Some(Category::Doc));
        assert_eq!(r, vec![f2]);

        // 路径模式：C:\photos（目录本身与其下文件都命中）
        let toks = parse_tokens("C:\\photos");
        let r = search(&ix, &toks, None);
        assert_eq!(r, vec![d, f1]);

        // 排除后不命中
        ix.recompute_exclusions(&["C:\\".into()]);
        let r = search(&ix, &parse_tokens("sunset"), None);
        assert!(r.is_empty());

        sort_results(&ix, &mut vec![2u32, 1, 0], SortKey::Name, false);
        assert!(contains_bytes(b"HelloWorld", b"World"));
    }
}

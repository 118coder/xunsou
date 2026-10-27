//! egui 主界面：检索工作台（顶栏 / 侧栏 / 结果表 / 检视面板 / 状态栏）。
//!
//! 结构对应 docs/ui-redesign.md：
//! - 顶栏 64：品牌 + 搜索框 + 结果元信息 + 视图开关
//! - 侧栏 208（可折 56）：分类 / 标记 / 索引范围
//! - 中央：表头 + 虚拟化结果表
//! - 右栏 320：预览 + 元信息 + 标记
//! - 底栏 26：状态 + 进度 + 计数 + 版本

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Color32, Rect, RichText, Sense, Stroke, Vec2, ViewportCommand, WidgetInfo, WidgetType};

use crate::categories::Category;
use crate::config::{self, Config};
use crate::indexer::{self, IndexHandle};
use crate::search::{self, SortKey};
use crate::ui::icons::{self, Icon};
use crate::ui::tokens::{self, Density, Pal, ThemePref};
use crate::ui::widgets;
use crate::win::{self, HotkeyThread};

pub const VERSION: &str = "1.0.0";

/// 索引条目少于该值时同步搜索，保证交互与无头测试的确定性；
/// 超过则交给后台线程，UI 线程永不阻塞。
const SYNC_SEARCH_LIMIT: usize = 50_000;

#[derive(Clone)]
enum Action {
    ToggleBatch(u32),
    Open(u32),
    OpenLoc(u32),
    Copy(String),
    Mark(Vec<u32>, Option<u8>),
    /// 移入回收站。u32 是索引条目下标，回收站语义由 win 层保证
    Delete(Vec<u32>),
    ClearMarks,
    SetCat(Category),
    Toast(String, bool),
}

enum PreviewPayload {
    /// 像素数据 + 尺寸 + 来源角标（如「视频首帧」）
    Image(Vec<u8>, [usize; 2], Option<&'static str>),
    Text(String),
    Nothing,
}

struct Preview {
    gen: u64,
    path: String,
    texture: Option<egui::TextureHandle>,
    caption: Option<&'static str>,
    text: Option<String>,
    loading: bool,
}

struct Draft {
    autostart: bool,
    hotkey: String,
    excluded: Vec<String>,
    sel_excl: Option<usize>,
    theme: ThemePref,
    density: Density,
}

/// 后台搜索任务的结果。
struct SearchDone {
    gen: u64,
    results: Arc<Vec<u32>>,
    counts: Option<[u32; 7]>,
    ms: f32,
    epoch: u64,
}

struct SearchReq {
    gen: u64,
    query: String,
    cat: Option<Category>,
    sort: SortKey,
    desc: bool,
    epoch: u64,
    narrow: Option<Arc<Vec<u32>>>,
    want_counts: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Breakpoint {
    Xs,
    S,
    M,
    L,
    Xl,
}

impl Breakpoint {
    fn of(w: f32) -> Self {
        if w < 680.0 {
            Breakpoint::Xs
        } else if w < 820.0 {
            Breakpoint::S
        } else if w < 980.0 {
            Breakpoint::M
        } else if w < 1180.0 {
            Breakpoint::L
        } else {
            Breakpoint::Xl
        }
    }
    fn has_sidebar(self) -> bool {
        self != Breakpoint::Xs
    }
    fn mini(self) -> bool {
        matches!(self, Breakpoint::S | Breakpoint::M)
    }
    fn has_preview(self) -> bool {
        self != Breakpoint::Xs
    }
    fn preview_w(self) -> f32 {
        match self {
            Breakpoint::M | Breakpoint::S => 264.0,
            Breakpoint::L => tokens::size::PREVIEW_MID,
            _ => tokens::size::PREVIEW,
        }
    }
}

/// 表头列宽（名称/路径为弹性，其余固定）。
struct Cols {
    check: f32,
    name: f32,
    path: f32,
    size: f32,
    time: f32,
    mark: f32,
    show_path: bool,
    show_time: bool,
    show_mark: bool,
}

impl Cols {
    /// 按可用宽度逐级降级：先丢标记列，再丢时间列，最后把路径下沉（窄屏走两行式）。
    fn layout(width: f32, bp: Breakpoint) -> Cols {
        let mut show_path = bp != Breakpoint::Xs;
        let mut show_time = bp != Breakpoint::Xs;
        let mut show_mark = bp != Breakpoint::Xs;
        // 六列全展示需要 ≈650；不足时按「标记 → 时间 → 路径」的顺序让位
        if width < 640.0 {
            show_mark = false;
        }
        if width < 500.0 {
            show_time = false;
        }
        if width < 400.0 {
            show_path = false;
        }
        let fixed = 32.0 + 88.0 + if show_time { 132.0 } else { 0.0 } + if show_mark { 28.0 } else { 0.0 };
        let flex = (width - fixed).max(140.0);
        let (name, path) = if show_path {
            // 名称通常短、路径长；5:5 比原来的 58:42 更少出现「路径只剩盘符」
            let name = (flex * 0.50).max(140.0);
            (name, (flex - name).max(110.0))
        } else {
            (flex, 0.0)
        };
        Cols { check: 32.0, name, path, size: 88.0, time: 132.0, mark: 28.0, show_path, show_time, show_mark }
    }
}

pub struct App {
    pub ctx: egui::Context,
    pub handle: IndexHandle,
    pub config: Config,
    pal: Pal,
    sys_dark: bool,
    bp: Breakpoint,
    // 搜索
    query: String,
    pub results: Arc<Vec<u32>>,
    results_epoch: u64,
    results_query: String,
    results_cat: Category,
    applied_query: String,
    applied_epoch: u64,
    applied_cat: Category,
    needs_search: bool,
    last_search: Instant,
    last_ms: f32,
    search_gen: u64,
    search_tx: Sender<SearchReq>,
    search_rx: Receiver<SearchDone>,
    searching: bool,
    counts: [u32; 7],
    counts_epoch: u64,
    // 状态
    pub cat: Category,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    pub selected: Option<u32>,
    batch: HashSet<u32>,
    // 预览
    preview: Option<Preview>,
    preview_tx: Option<Sender<(u64, PreviewPayload)>>,
    preview_rx: Receiver<(u64, PreviewPayload)>,
    preview_gen: u64,
    // UI
    right_open: bool,
    settings_open: bool,
    draft: Draft,
    hotkey: Option<HotkeyThread>,
    hotkey_rx: Receiver<()>,
    hidden: bool,
    pending_copy: Option<String>,
    toast: Option<(String, Instant, bool)>,
    focus_search: bool,
    search_focused: bool,
    is_admin: bool,
    fonts_set: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = config::load();
        indexer::set_allow_walk(config.allow_walk);
        let handle = indexer::startup(config.excluded.clone(), cc.egui_ctx.clone());
        let mut app = App::with_parts(cc.egui_ctx.clone(), config, handle);
        if let Some((h, rx)) = HotkeyThread::start(&app.config.hotkey) {
            app.hotkey = Some(h);
            app.hotkey_rx = rx;
        }
        app
    }

    /// 无头构造入口：给定 ctx / 配置 / 索引句柄，不启动真实索引线程与热键线程。
    /// 供 kittest 无头测试与未来 CLI 复用。
    pub fn with_parts(ctx: egui::Context, config: Config, handle: IndexHandle) -> Self {
        setup_fonts(&ctx);
        let sys_dark = ctx.style().visuals.dark_mode;
        let pal = tokens::pal(config.ui.theme, sys_dark);
        tokens::apply(&ctx, &pal, config.ui.density);
        // 预热 rayon 全局线程池，避免首次搜索把池初始化时间算进耗时
        rayon::spawn(|| {});
        let (_, hotkey_rx) = channel();
        let (tx, rx) = channel::<(u64, PreviewPayload)>();
        let right_open = config.ui.preview_open;

        // 后台搜索线程：只在索引规模较大时启用
        let (req_tx, req_rx) = channel::<SearchReq>();
        let (done_tx, done_rx) = channel::<SearchDone>();
        let index = handle.index.clone();
        let ctx2 = ctx.clone();
        std::thread::Builder::new()
            .name("search".into())
            .spawn(move || {
                while let Ok(req) = req_rx.recv() {
                    let t0 = Instant::now();
                    let tokens = search::parse_tokens(&req.query);
                    let (mut res, counts) = {
                        let ix = index.read().unwrap();
                        let res = match &req.narrow {
                            Some(prev) => search::search_within(&ix, prev, &tokens, req.cat),
                            None => search::search(&ix, &tokens, req.cat),
                        };
                        let counts = if req.want_counts { Some(search::count_by_category(&ix)) } else { None };
                        (res, counts)
                    };
                    {
                        let ix = index.read().unwrap();
                        if req.sort != SortKey::None {
                            search::sort_results(&ix, &mut res, req.sort, req.desc);
                        }
                    }
                    let _ = done_tx.send(SearchDone {
                        gen: req.gen,
                        results: Arc::new(res),
                        counts,
                        ms: t0.elapsed().as_secs_f32() * 1000.0,
                        epoch: req.epoch,
                    });
                    ctx2.request_repaint();
                }
            })
            .ok();

        App {
            ctx,
            handle,
            config,
            pal,
            sys_dark,
            bp: Breakpoint::L,
            query: String::new(),
            results: Arc::new(Vec::new()),
            results_epoch: u64::MAX,
            results_query: "\u{0}init".into(),
            results_cat: Category::All,
            applied_query: String::new(),
            applied_epoch: u64::MAX,
            applied_cat: Category::All,
            needs_search: true,
            last_search: Instant::now() - Duration::from_secs(10),
            last_ms: 0.0,
            search_gen: 0,
            search_tx: req_tx,
            search_rx: done_rx,
            searching: false,
            counts: [0; 7],
            counts_epoch: u64::MAX,
            cat: Category::All,
            sort_key: SortKey::Name,
            sort_desc: false,
            selected: None,
            batch: HashSet::new(),
            preview: None,
            preview_tx: Some(tx),
            preview_rx: rx,
            preview_gen: 0,
            right_open,
            settings_open: false,
            draft: Draft {
                autostart: false,
                hotkey: String::new(),
                excluded: vec![],
                sel_excl: None,
                theme: ThemePref::System,
                density: Density::Normal,
            },
            hotkey: None,
            hotkey_rx,
            hidden: false,
            pending_copy: None,
            toast: None,
            // 启动即聚焦搜索框：唤起即可输入，与设计稿一致
            focus_search: true,
            search_focused: false,
            is_admin: win::is_admin(),
            fonts_set: false,
        }
    }

    fn apply_look(&mut self) {
        self.pal = tokens::pal(self.config.ui.theme, self.sys_dark);
        tokens::apply(&self.ctx, &self.pal, self.config.ui.density);
    }

    fn toast(&mut self, msg: impl Into<String>, ok: bool) {
        self.toast = Some((msg.into(), Instant::now(), ok));
    }

    fn cat_opt(&self) -> Option<Category> {
        if self.cat == Category::All {
            None
        } else {
            Some(self.cat)
        }
    }

    // ---------- 搜索 ----------

    fn ensure_search(&mut self) {
        let index = self.handle.index.clone();
        let (epoch, ready, total) = {
            let ix = index.read().unwrap();
            (ix.epoch, self.handle.status.get().ready, ix.entries.len())
        };
        let query_changed = self.query != self.results_query || self.cat != self.results_cat;
        let epoch_stale = ready && epoch != self.results_epoch;
        // 后台索引变化驱动的自动重搜做节流；用户操作（输入/点击）立即生效
        let background_ok = self.last_search.elapsed() > Duration::from_millis(400);
        if !(query_changed || self.needs_search || (epoch_stale && background_ok)) {
            return;
        }
        self.last_search = Instant::now();
        self.needs_search = false;

        let want_counts = self.counts_epoch != epoch;

        if total <= SYNC_SEARCH_LIMIT {
            let t0 = Instant::now();
            let tokens = search::parse_tokens(&self.query);
            let cat = self.cat_opt();
            let narrow = self.narrow_source(epoch);
            let (mut res, counts) = {
                let ix = index.read().unwrap();
                let res = match &narrow {
                    Some(prev) => search::search_within(&ix, prev, &tokens, cat),
                    None => search::search(&ix, &tokens, cat),
                };
                let counts = search::count_by_category(&ix);
                (res, counts)
            };
            {
                let ix = index.read().unwrap();
                if self.sort_key != SortKey::None {
                    search::sort_results(&ix, &mut res, self.sort_key, self.sort_desc);
                }
            }
            self.last_ms = t0.elapsed().as_secs_f32() * 1000.0;
            self.results = Arc::new(res);
            self.counts = counts;
            self.counts_epoch = epoch;
            self.finish_search(epoch);
            return;
        }

        // 大索引：交给后台线程，先保留旧结果
        self.search_gen += 1;
        let narrow = self.narrow_source(epoch);
        let req = SearchReq {
            gen: self.search_gen,
            query: self.query.clone(),
            cat: self.cat_opt(),
            sort: self.sort_key,
            desc: self.sort_desc,
            epoch,
            narrow,
            want_counts,
        };
        if self.search_tx.send(req).is_ok() {
            self.searching = true;
            self.results_query = self.query.clone();
            self.results_cat = self.cat;
            self.results_epoch = epoch;
        }
    }

    /// 若新查询只是旧查询的后缀追加，返回上次结果用于收窄。
    fn narrow_source(&self, epoch: u64) -> Option<Arc<Vec<u32>>> {
        if epoch != self.applied_epoch || self.cat != self.applied_cat {
            return None;
        }
        if self.query.len() > self.applied_query.len() && self.query.starts_with(&self.applied_query) {
            Some(self.results.clone())
        } else {
            None
        }
    }

    fn poll_search(&mut self) {
        while let Ok(done) = self.search_rx.try_recv() {
            if done.gen != self.search_gen {
                continue;
            }
            self.results = done.results;
            self.counts_epoch = done.epoch;
            if let Some(c) = done.counts {
                self.counts = c;
            }
            self.last_ms = done.ms;
            self.searching = false;
            self.finish_search(done.epoch);
        }
    }

    fn finish_search(&mut self, epoch: u64) {
        self.results_epoch = epoch;
        self.results_query = self.query.clone();
        self.results_cat = self.cat;
        self.applied_query = self.query.clone();
        self.applied_epoch = epoch;
        self.applied_cat = self.cat;
        let n = self.results.len() as u32;
        if let Some(s) = self.selected {
            if s >= n {
                self.selected = None;
            }
        }
        self.batch.retain(|&i| i < n);
    }

    // ---------- 预览 ----------

    fn maybe_load_preview(&mut self) {
        let want = match self.selected {
            Some(i) => self.results.get(i as usize).copied(),
            None => None,
        };
        let target = want.map(|entry_idx| {
            let ix = self.handle.index.read().unwrap();
            let path = ix.path_of(entry_idx);
            let e = &ix.entries[entry_idx as usize];
            (path, e.size, e.mtime, e.is_dir())
        });
        let sel_gen = self.selected.map(|i| i as u64 + 1).unwrap_or(0);
        match (&self.preview, &target) {
            (Some(p), Some((path, _, _, _))) if p.path == *path && p.gen == sel_gen => {}
            _ => {
                self.preview_gen += 1;
                match target {
                    Some((path, _size, _mtime, is_dir)) => {
                        let gen = self.preview_gen;
                        if let Some(tx) = self.preview_tx.clone() {
                            spawn_preview(path.clone(), gen, tx, is_dir);
                        }
                        self.preview =
                            Some(Preview { gen: sel_gen, path, texture: None, caption: None, text: None, loading: true });
                    }
                    None => self.preview = None,
                }
            }
        }
        while let Ok((gen, payload)) = self.preview_rx.try_recv() {
            if gen != self.preview_gen {
                continue;
            }
            if let Some(p) = self.preview.as_mut() {
                p.loading = false;
                match payload {
                    PreviewPayload::Image(rgba, size, caption) => {
                        let img = egui::ColorImage::from_rgba_unmultiplied([size[0], size[1]], &rgba);
                        p.texture = Some(self.ctx.load_texture("preview", img, egui::TextureOptions::LINEAR));
                        p.caption = caption;
                    }
                    PreviewPayload::Text(t) => p.text = Some(t),
                    PreviewPayload::Nothing => {}
                }
            }
        }
    }

    fn mark_targets(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.batch.iter().copied().collect();
        if let Some(s) = self.selected {
            if let Some(&ei) = self.results.get(s as usize) {
                if !v.contains(&ei) {
                    v.push(ei);
                }
            }
        }
        v.sort_unstable();
        v
    }

    fn path_of_entry(&self, i: u32) -> Option<String> {
        let ix = self.handle.index.read().unwrap();
        if (i as usize) < ix.entries.len() {
            Some(ix.path_of(i))
        } else {
            None
        }
    }

    // ---------- 动作 ----------

    fn apply_action(&mut self, a: Action) {
        match a {
            Action::ToggleBatch(i) => {
                if !self.batch.remove(&i) {
                    self.batch.insert(i);
                }
            }
            Action::Open(i) => {
                if let Some(path) = self.path_of_entry(i) {
                    if std::path::Path::new(&path).exists() {
                        let _ = open::that(&path);
                    } else {
                        self.toast("文件不存在，可能已被移动或删除", false);
                    }
                }
            }
            Action::OpenLoc(i) => {
                if let Some(path) = self.path_of_entry(i) {
                    win::show_in_explorer(&path);
                }
            }
            Action::Copy(s) => {
                self.pending_copy = Some(s);
                self.toast("已复制路径", true);
            }
            Action::Delete(items) => {
                let n = items.len();
                let paths: Vec<String> = items
                    .iter()
                    .filter_map(|i| self.path_of_entry(*i))
                    .filter(|p| std::path::Path::new(p).exists())
                    .collect();
                if paths.is_empty() {
                    self.toast("文件不存在，可能已被移动或删除", false);
                    return;
                }
                match win::delete_to_recycle_bin(&paths) {
                    Ok(()) => {
                        // 立即从结果里消失：打上排除标记（重建索引后自然不再出现）
                        let mut ix = self.handle.index.write().unwrap();
                        for i in items {
                            if let Some(e) = ix.entries.get_mut(i as usize) {
                                e.flags |= crate::model::FLAG_EXCLUDED;
                            }
                        }
                        drop(ix);
                        self.needs_search = true;
                        self.selected = None;
                        self.batch.clear();
                        self.toast(format!("已移入回收站 {} 项", paths.len()), true);
                    }
                    Err(e) => self.toast(format!("删除失败（{e}），文件可能被占用"), false),
                }
            }
            Action::Mark(items, color) => {
                let n = items.len();
                for i in items {
                    if let Some(path) = self.path_of_entry(i) {
                        match color {
                            Some(c) => {
                                self.config.marks.insert(path, c);
                            }
                            None => {
                                self.config.marks.remove(&path);
                            }
                        }
                    }
                }
                config::save(&self.config);
                self.toast(if color.is_some() { format!("已标记 {n} 项") } else { "已移除标记".into() }, true);
            }
            Action::ClearMarks => {
                self.config.marks.clear();
                config::save(&self.config);
                self.toast("已清除全部标记", true);
            }
            Action::SetCat(c) => {
                self.cat = c;
                self.needs_search = true;
            }
            Action::Toast(msg, ok) => self.toast(msg, ok),
        }
    }

    fn open_settings(&mut self) {
        self.draft = Draft {
            autostart: self.config.autostart,
            hotkey: self.config.hotkey.clone(),
            excluded: self.config.excluded.clone(),
            sel_excl: None,
            theme: self.config.ui.theme,
            density: self.config.ui.density,
        };
        self.settings_open = true;
    }

    fn apply_settings(&mut self) {
        self.config.autostart = self.draft.autostart;
        let _ = config::set_autostart(self.draft.autostart);
        if self.draft.hotkey != self.config.hotkey {
            if let Some(h) = &self.hotkey {
                h.stop();
            }
            self.hotkey = None;
            if let Some((h, rx)) = HotkeyThread::start(&self.draft.hotkey) {
                self.hotkey = Some(h);
                self.hotkey_rx = rx;
            }
            self.config.hotkey = self.draft.hotkey.clone();
        }
        if self.draft.excluded != self.config.excluded {
            self.config.excluded = self.draft.excluded.clone();
            let _ = self.handle.excl_tx.send(self.config.excluded.clone());
        }
        if self.draft.theme != self.config.ui.theme || self.draft.density != self.config.ui.density {
            self.config.ui.theme = self.draft.theme;
            self.config.ui.density = self.draft.density;
            self.apply_look();
        }
        config::save(&self.config);
        self.toast("设置已保存", true);
    }

    fn take_copy(&mut self) -> Option<String> {
        self.pending_copy.take()
    }

    // ---------- 键位 ----------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let typing = ctx.memory(|m| m.focused()).is_some();

        // ---- 全局组合键 ----
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::F) || i.consume_key(Modifiers::NONE, Key::F3)) {
            self.focus_search = true;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Comma)) {
            self.open_settings();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::R)) {
            indexer::request_refresh(&self.handle);
            self.toast("正在重建索引…", true);
        }
        for (k, c) in [
            (Key::Num1, Category::All),
            (Key::Num2, Category::Image),
            (Key::Num3, Category::Doc),
            (Key::Num4, Category::Video),
            (Key::Num5, Category::Audio),
            (Key::Num6, Category::Phone),
            (Key::Num7, Category::Dir),
        ] {
            if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, k)) {
                self.cat = c;
                self.needs_search = true;
            }
        }

        // ---- 导航键：单行输入框不使用方向键，焦点在搜索框里也应当能用 ----
        let n = self.results.len() as i64;
        if n > 0 {
            let cur = self.selected.map(|s| s as i64);
            let mut next = cur;
            if ctx.input(|i| i.key_pressed(Key::ArrowDown)) {
                next = Some(cur.map(|c| (c + 1).min(n - 1)).unwrap_or(0));
            }
            if ctx.input(|i| i.key_pressed(Key::ArrowUp)) {
                next = Some(cur.map(|c| (c - 1).max(0)).unwrap_or(0));
            }
            if ctx.input(|i| i.key_pressed(Key::PageDown)) {
                next = Some(cur.map(|c| (c + 15).min(n - 1)).unwrap_or(0));
            }
            if ctx.input(|i| i.key_pressed(Key::PageUp)) {
                next = Some(cur.map(|c| (c - 15).max(0)).unwrap_or(0));
            }
            if ctx.input(|i| i.key_pressed(Key::Home)) {
                next = Some(0);
            }
            if ctx.input(|i| i.key_pressed(Key::End)) {
                next = Some(n - 1);
            }
            if let Some(nx) = next {
                if Some(nx as u32) != self.selected {
                    self.selected = Some(nx as u32);
                }
            }
            if ctx.input(|i| i.key_pressed(Key::Space)) && !typing {
                if let Some(sel) = self.selected {
                    if let Some(&ei) = self.results.get(sel as usize) {
                        self.apply_action(Action::ToggleBatch(ei));
                    }
                }
            }
        }

        // ---- Esc 逐级退出：清空搜索 → 收起预览 → 取消选中 ----
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if !self.query.is_empty() {
                self.query.clear();
                self.needs_search = true;
            } else if self.right_open && !self.bp.mini() {
                self.right_open = false;
                self.config.ui.preview_open = false;
                config::save(&self.config);
            } else {
                self.selected = None;
            }
            return;
        }

        if typing {
            // 搜索框内回车：打开当前选中项，没有选中则打开第一条
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                if let Some(sel) = self.selected.or(Some(0)) {
                    if let Some(&ei) = self.results.get(sel as usize) {
                        self.apply_action(Action::Open(ei));
                    }
                }
            }
            return;
        }

        // ---- 表格焦点下的组合键 ----
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::A)) {
            self.batch = self.results.iter().copied().collect();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::C)) {
            if let Some(sel) = self.selected {
                if let Some(&ei) = self.results.get(sel as usize) {
                    if let Some(path) = self.path_of_entry(ei) {
                        self.apply_action(Action::Copy(path));
                    }
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Enter)) {
            if let Some(sel) = self.selected {
                if let Some(&ei) = self.results.get(sel as usize) {
                    self.apply_action(Action::OpenLoc(ei));
                }
            }
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            if let Some(sel) = self.selected {
                if let Some(&ei) = self.results.get(sel as usize) {
                    self.apply_action(Action::Open(ei));
                }
            }
        }
    }
}

// ============================ 字体 ============================

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let win_fonts = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    for cand in ["msyh.ttc", "simhei.ttf", "deng.ttf", "simsun.ttc"] {
        let p = std::path::Path::new(&win_fonts).join("Fonts").join(cand);
        if let Ok(data) = std::fs::read(&p) {
            if data.len() > 1_000_000 {
                fonts.font_data.insert("cjk".into(), egui::FontData::from_owned(data).into());
                fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "cjk".into());
                fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".into());
                break;
            }
        }
    }
    ctx.set_fonts(fonts);
}

// ============================ 预览加载线程 ============================

fn spawn_preview(path: String, gen: u64, tx: Sender<(u64, PreviewPayload)>, is_dir: bool) {
    std::thread::Builder::new()
        .name("preview".into())
        .spawn(move || {
            if is_dir {
                let _ = tx.send((gen, PreviewPayload::Nothing));
                return;
            }
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();

            // ① 位图：自己解码，质量与速度都优于系统缩略图
            let img_exts = ["png", "jpg", "jpeg", "gif", "bmp", "webp"];
            if img_exts.contains(&ext.as_str()) && size > 0 && size < 80 * 1024 * 1024 {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(mut img) = image::load_from_memory(&bytes) {
                        let maxdim = 1600;
                        if img.width() > maxdim || img.height() > maxdim {
                            img = img.thumbnail(maxdim, maxdim);
                        }
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width() as usize, rgba.height() as usize);
                        let _ = tx.send((gen, PreviewPayload::Image(rgba.into_raw(), [w, h], None)));
                        return;
                    }
                }
                // 解码失败不急着放弃：HEIC/RAW 之类交给系统缩略图
            }

            // ② 文本：直接读前 128KB
            let text_exts = [
                "txt", "md", "log", "ini", "cfg", "json", "xml", "yml", "yaml", "html", "htm", "css", "js", "py",
                "c", "cpp", "h", "java", "csv", "tsv", "bat", "ps1", "rs", "go", "sql", "toml", "srt", "lrc",
            ];
            if size < 4 * 1024 * 1024
                && (text_exts.contains(&ext.as_str()) || (!path.contains('.') && size < 256 * 1024))
            {
                use std::io::Read;
                if let Ok(mut f) = std::fs::File::open(&path) {
                    let mut buf = vec![0u8; 128 * 1024];
                    let n = f.read(&mut buf).unwrap_or(0);
                    buf.truncate(n);
                    if !buf.contains(&0) {
                        let t = String::from_utf8_lossy(&buf).into_owned();
                        let _ = tx.send((gen, PreviewPayload::Text(t)));
                        return;
                    }
                }
            }

            // ③ 其余全部交给 Windows 缩略图：视频取首帧，PDF / Office / HEIC 同理
            let video_exts = [
                "mp4", "mkv", "avi", "mov", "wmv", "flv", "m4v", "webm", "ts", "m2ts", "rmvb", "rm", "mpg", "mpeg",
                "3gp", "vob", "ogv", "asf", "divx",
            ];
            let is_video = video_exts.contains(&ext.as_str());
            if is_video || size > 0 {
                if let Some((rgba, w, h)) = crate::thumb::shell_thumbnail(&path, 1024) {
                    let cap = if is_video { Some("视频首帧") } else { None };
                    let _ = tx.send((gen, PreviewPayload::Image(rgba, [w, h], cap)));
                    return;
                }
            }
            let _ = tx.send((gen, PreviewPayload::Nothing));
        })
        .ok();
}

// ============================ 帧渲染（无头可测） ============================

impl App {
    /// 渲染一帧 UI。eframe 主循环与 kittest 无头测试共用此入口。
    pub fn update_frame(&mut self, ctx: &egui::Context) {
        self.ctx = ctx.clone();
        if !self.fonts_set {
            setup_fonts(ctx);
            self.apply_look();
            self.fonts_set = true;
        }
        // 全局热键
        while self.hotkey_rx.try_recv().is_ok() {
            self.hidden = !self.hidden;
            ctx.send_viewport_cmd(ViewportCommand::Visible(!self.hidden));
            if !self.hidden {
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }

        self.bp = Breakpoint::of(ctx.screen_rect().width());
        self.handle_keys(ctx);
        self.poll_search();
        self.ensure_search();
        self.maybe_load_preview();

        let p = self.pal;
        let mut actions: Vec<Action> = Vec::new();

        self.top_bar(ctx, &p, &mut actions);
        self.status_bar(ctx, &p, &mut actions);
        if self.bp.has_sidebar() {
            self.sidebar(ctx, &p, &mut actions);
        }
        if self.right_open && self.bp.has_preview() {
            self.preview_panel(ctx, &p, &mut actions);
        }
        self.central(ctx, &p, &mut actions);
        self.settings_window(ctx, &p);

        for a in actions {
            self.apply_action(a);
        }
        self.toast_overlay(ctx, &p);
        if let Some(s) = self.take_copy() {
            ctx.output_mut(|o| o.copied_text = s);
        }
    }

    // ---------- 顶栏 ----------

    fn top_bar(&mut self, ctx: &egui::Context, p: &Pal, actions: &mut Vec<Action>) {
        egui::TopBottomPanel::top("top_bar")
            .exact_height(tokens::size::TOPBAR)
            .frame(egui::Frame::none().fill(p.bg).inner_margin(egui::Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // 品牌标记
                    let (logo, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter().rect_filled(logo, 7.0, p.brand);
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(logo.center(), Vec2::splat(15.0)),
                        Icon::Bolt,
                        p.on_brand,
                        1.7,
                    );
                    ui.add_space(4.0);

                    // 右侧簇按实际字宽预留：搜索框优先，空间不足时先让出元信息
                    let n_btn: f32 = if self.bp.has_sidebar() { 2.0 } else { 3.0 };
                    let btns_w = n_btn * 28.0 + (n_btn - 1.0) * 8.0;
                    let meta = if self.last_ms < 1.0 {
                        format!("{} 条 · <1 ms", self.results.len())
                    } else {
                        format!("{} 条 · {:.0} ms", self.results.len(), self.last_ms)
                    };
                    let meta_w = ui
                        .painter()
                        .layout_no_wrap(meta.clone(), egui::FontId::monospace(11.5), p.text3)
                        .size()
                        .x;
                    let avail = ui.available_width();
                    let show_meta = avail - 200.0 - btns_w - 24.0 > meta_w;
                    let need = btns_w + 8.0 + if show_meta { meta_w + 8.0 } else { 0.0 };
                    // 输入框超宽会稀释视觉重心，也难扫读；超过 640 后把余量让给留白，
                    // 右侧簇仍然贴边，形成「左输入 / 右信息」的两端对齐。
                    // 搜索框吃掉剩余空间，计数与图标贴右成组——中间不留空洞，
                    // 「输入 → 结果量 → 视图操作」是一条连续的阅读带。
                    let w = (avail - need - 8.0).max(200.0);
                    self.search_field(ui, p, w);
                    if show_meta {
                        ui.add_space(6.0);
                        ui.label(RichText::new(meta).size(11.5).color(p.text3).monospace());
                    }

                    if widgets::icon_button(ui, p, Icon::Grid, "行高密度", false, 28.0).clicked() {
                        self.config.ui.density = match self.config.ui.density {
                            Density::Compact => Density::Normal,
                            Density::Normal => Density::Comfortable,
                            Density::Comfortable => Density::Compact,
                        };
                        config::save(&self.config);
                        self.apply_look();
                    }
                    if widgets::icon_button(ui, p, Icon::Panel, "预览面板", self.right_open, 28.0).clicked() {
                        self.right_open = !self.right_open;
                        self.config.ui.preview_open = self.right_open;
                        config::save(&self.config);
                    }
                    if !self.bp.has_sidebar()
                        && widgets::icon_button(ui, p, Icon::Settings, "设置", false, 28.0).clicked()
                    {
                        self.open_settings();
                    }
                    let _ = actions;
                });
            });
    }

    fn search_field(&mut self, ui: &mut egui::Ui, p: &Pal, w: f32) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, tokens::size::SEARCH_H), Sense::hover());
        let focused = self.search_focused;
        ui.painter().rect_filled(rect.expand(2.0), 8.0, if focused { p.brand_soft } else { Color32::TRANSPARENT });
        ui.painter().rect_filled(rect, tokens::size::RADIUS, p.surface);
        ui.painter().rect_stroke(
            rect,
            tokens::size::RADIUS,
            Stroke::new(1.0, if focused { p.brand } else { p.border2 }),
        );
        icons::paint(
            ui.painter(),
            Rect::from_center_size(egui::pos2(rect.left() + 14.0, rect.center().y), Vec2::splat(15.0)),
            Icon::Search,
            if focused { p.brand } else { p.text3 },
            1.5,
        );

        // 右侧需要预留的宽度：有内容时给清除按钮，空时给键帽提示（按实际字宽量）
        let hint = "Alt + `";
        let show_hint = self.query.is_empty() && w > 380.0;
        let hint_w = if show_hint {
            ui.painter()
                .layout_no_wrap(hint.to_owned(), egui::FontId::proportional(10.5), p.text3)
                .size()
                .x
                + 12.0
        } else {
            0.0
        };
        let right_pad = if show_hint { hint_w + 12.0 } else { 28.0 };

        let inner = Rect::from_min_max(
            egui::pos2(rect.left() + 26.0, rect.top() + 5.0),
            egui::pos2(rect.right() - right_pad, rect.bottom() - 5.0),
        );
        // 关键：文本编辑放在独立子 UI 里，子 UI 的布局不会回退父级的横向游标
        let mut edit_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let resp = edit_ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .frame(false)
                .hint_text("搜索文件与文件夹，空格分隔多个关键词")
                .text_color(p.text)
                .font(egui::FontId::proportional(13.0)),
        );
        if resp.changed() {
            self.needs_search = true;
        }
        if self.focus_search {
            resp.request_focus();
            self.focus_search = false;
        }
        self.search_focused = resp.has_focus();

        if !self.query.is_empty() {
            let cr = Rect::from_center_size(egui::pos2(rect.right() - 16.0, rect.center().y), Vec2::splat(18.0));
            let cresp = ui.interact(cr, ui.id().with("clr"), Sense::click());
            if cresp.hovered() {
                ui.painter().rect_filled(cr, 4.0, p.hover);
            }
            icons::paint(ui.painter(), Rect::from_center_size(cr.center(), Vec2::splat(12.0)), Icon::Close, p.text3, 1.7);
            cresp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "清除搜索"));
            if cresp.clicked() {
                self.query.clear();
                self.needs_search = true;
                self.focus_search = true;
            }
        } else if show_hint {
            let galley = ui
                .painter()
                .layout_no_wrap(hint.to_owned(), egui::FontId::proportional(10.5), p.text3);
            let kr = Rect::from_center_size(
                egui::pos2(rect.right() - 8.0 - hint_w / 2.0, rect.center().y),
                Vec2::new(hint_w, 17.0),
            );
            ui.painter().rect_filled(kr, 4.0, p.bg);
            ui.painter().rect_stroke(kr, 4.0, Stroke::new(1.0, p.border2));
            ui.painter().galley(
                egui::pos2(kr.center().x - galley.size().x / 2.0, kr.center().y - galley.size().y / 2.0),
                galley,
                p.text3,
            );
        }
    }

    // ---------- 状态栏 ----------

    fn status_bar(&mut self, ctx: &egui::Context, p: &Pal, actions: &mut Vec<Action>) {
        let st = self.handle.status.get();
        let total = self.handle.index.read().unwrap().entries.len();
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(tokens::size::STATUS)
            .frame(egui::Frame::none().fill(p.surface).inner_margin(egui::Margin::symmetric(12.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    let color = if st.error.is_some() {
                        p.danger
                    } else if st.ready {
                        p.ok
                    } else {
                        p.warn
                    };
                    ui.painter().circle_filled(dot.center(), 3.0, color);
                    let phase_color = if st.error.is_some() { p.danger } else { p.text2 };
                    let phase_text = match &st.error {
                        Some(e) => elide(e, 46),
                        None => st.phase.clone(),
                    };
                    ui.label(RichText::new(phase_text).size(11.5).color(phase_color));

                    if !st.ready && st.volumes_total > 0 {
                        let (br, _) = ui.allocate_exact_size(Vec2::new(88.0, 3.0), Sense::hover());
                        let frac = (st.volumes_done as f32 / st.volumes_total as f32).clamp(0.0, 1.0);
                        ui.painter().rect_filled(br, 2.0, p.border);
                        if frac > 0.0 {
                            let filled = Rect::from_min_size(br.min, Vec2::new(br.width() * frac, br.height()));
                            ui.painter().rect_filled(filled, 2.0, p.brand);
                        }
                    }
                    ui.label(
                        RichText::new(format!("总文件/文件夹 {} 个", total))
                            .size(11.5)
                            .color(p.text3),
                    );
                    if !self.is_admin && !st.mft_mode {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(96.0, 18.0), Sense::click());
                        ui.painter().rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(245, 165, 36, 34));
                        icons::paint(
                            ui.painter(),
                            Rect::from_center_size(egui::pos2(r.left() + 11.0, r.center().y), Vec2::splat(11.0)),
                            Icon::Shield,
                            p.warn,
                            1.4,
                        );
                        ui.painter().text(
                            egui::pos2(r.left() + 20.0, r.center().y),
                            egui::Align2::LEFT_CENTER,
                            "启用极速索引",
                            egui::FontId::proportional(11.0),
                            p.warn,
                        );
                        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "启用极速索引"));
                        if resp.clicked() {
                            win::restart_elevated();
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("v{VERSION}")).size(11.0).color(p.text3));
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(if self.searching { "搜索中…".into() } else {
                                format!("{}", if st.ready { "就绪" } else { "索引中" })
                            })
                            .size(11.0)
                            .color(p.text3),
                        );
                    });
                    let _ = actions;
                });
            });
    }

    // ---------- 侧栏 ----------

    fn sidebar(&mut self, ctx: &egui::Context, p: &Pal, actions: &mut Vec<Action>) {
        let compact = self.config.ui.sidebar_collapsed || self.bp.mini();
        let w = if compact { tokens::size::SIDEBAR_MINI } else { tokens::size::SIDEBAR };
        egui::SidePanel::left("side_bar")
            .exact_width(w)
            .resizable(false)
            .frame(egui::Frame::none().fill(p.surface).inner_margin(egui::Margin::symmetric(8.0, 8.0)))
            .show(ctx, |ui| {
                // 折叠态底部是竖排三个 28px 按钮，需要预留约 104px
                let foot_h = if compact { 104.0 } else { 46.0 };
                let list_h = (ui.available_height() - foot_h).max(80.0);
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .id_salt("side_scroll")
                    .show(ui, |ui| {
                        if !compact {
                            widgets::section(ui, p, "分类");
                        }
                        for c in Category::ALL {
                            self.nav_item(ui, p, c, compact, actions);
                        }
                        if !compact {
                            widgets::divider(ui, p);
                            widgets::section(ui, p, "标记");
                            let marked = self.config.marks.len();
                            ui.horizontal(|ui| {
                                ui.add_space(8.0);
                                ui.label(RichText::new(format!("已标记 {marked} 项")).size(11.5).color(p.text3));
                            });
                            ui.horizontal(|ui| {
                                ui.add_space(8.0);
                                for c in tokens::MARKS {
                                    widgets::dot(ui, c, 14.0);
                                }
                            });
                            widgets::divider(ui, p);
                            widgets::section(ui, p, "索引范围");
                            let vols = self.handle.index.read().unwrap().volumes.clone();
                            for v in vols {
                                ui.horizontal(|ui| {
                                    ui.add_space(8.0);
                                    icons::image(ui, Icon::Disk, 14.0, p.text3);
                                    let kind = match v.kind {
                                        crate::model::VolumeKind::Mft => "MFT",
                                        crate::model::VolumeKind::Walk => "遍历",
                                    };
                                    ui.label(
                                        RichText::new(format!("{}:  {}", v.letter, kind))
                                            .size(12.0)
                                            .color(p.text2),
                                    );
                                });
                            }
                        }
                    });

                // 底部工具条
                ui.add_space(4.0);
                let (line, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
                ui.painter().line_segment(
                    [line.left_center(), line.right_center()],
                    Stroke::new(1.0, p.border),
                );
                ui.add_space(4.0);
                let mut footer = |ui: &mut egui::Ui| {
                    let theme_icon = if p.dark { Icon::Moon } else { Icon::Sun };
                    if widgets::icon_button(ui, p, theme_icon, "切换主题", false, 28.0).clicked() {
                        self.config.ui.theme = match self.config.ui.theme {
                            ThemePref::System => {
                                if p.dark {
                                    ThemePref::Light
                                } else {
                                    ThemePref::Dark
                                }
                            }
                            ThemePref::Dark => ThemePref::Light,
                            ThemePref::Light => ThemePref::Dark,
                        };
                        config::save(&self.config);
                        self.apply_look();
                    }
                    if widgets::icon_button(ui, p, Icon::Settings, "设置", false, 28.0).clicked() {
                        self.open_settings();
                    }
                    if compact {
                        if widgets::icon_button(ui, p, Icon::Refresh, "重建索引", false, 28.0).clicked() {
                            indexer::request_refresh(&self.handle);
                            self.toast("正在重建索引…", true);
                        }
                    } else {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if widgets::icon_button(ui, p, Icon::Refresh, "重建索引", false, 28.0).clicked() {
                                indexer::request_refresh(&self.handle);
                                self.toast("正在重建索引…", true);
                            }
                        });
                    }
                };
                if compact {
                    ui.vertical(|ui| footer(ui));
                } else {
                    ui.horizontal(|ui| footer(ui));
                }
            });
    }

    fn nav_item(&mut self, ui: &mut egui::Ui, p: &Pal, c: Category, compact: bool, actions: &mut Vec<Action>) {
        let h = 32.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
        let active = self.cat == c;
        resp.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, active, c.label()));

        if active {
            ui.painter().rect_filled(rect, tokens::size::RADIUS, p.brand_soft);
            ui.painter().rect_filled(
                Rect::from_min_size(rect.min + Vec2::new(0.0, 7.0), Vec2::new(2.0, h - 14.0)),
                1.0,
                p.brand,
            );
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, tokens::size::RADIUS, p.hover);
        }

        let cx = if compact { rect.center().x } else { rect.left() + 22.0 };
        let cat_color = tokens::CAT_COLORS[icons::cat_index(c)];
        icons::paint(
            ui.painter(),
            Rect::from_center_size(egui::pos2(cx, rect.center().y), Vec2::splat(16.0)),
            icons::for_category(c),
            if active { p.brand } else { cat_color },
            1.5,
        );

        if !compact {
            ui.painter().text(
                egui::pos2(rect.left() + 40.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                c.label(),
                egui::FontId::proportional(13.0),
                if active { p.text } else { p.text2 },
            );
            let n = self.counts[icons::cat_index(c)];
            if n > 0 {
                ui.painter().text(
                    egui::pos2(rect.right() - 8.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    fmt_count(n),
                    egui::FontId::monospace(10.5),
                    if active { p.brand_text } else { p.text3 },
                );
            }
        } else {
            resp.clone().on_hover_text(c.label());
        }

        if resp.clicked() {
            actions.push(Action::SetCat(c));
        }
    }

    // ---------- 检视面板 ----------

    fn preview_panel(&mut self, ctx: &egui::Context, p: &Pal, actions: &mut Vec<Action>) {
        let w = self.bp.preview_w();
        egui::SidePanel::right("preview_bar")
            .exact_width(w)
            .resizable(false)
            .frame(egui::Frame::none().fill(p.surface))
            .show(ctx, |ui| {
                let sel_entry = self.selected.and_then(|i| self.results.get(i as usize).copied());
                let meta = sel_entry.map(|ei| {
                    let ix = self.handle.index.read().unwrap();
                    let e = &ix.entries[ei as usize];
                    (ix.path_of(ei), e.size, e.mtime, e.is_dir(), ix.name_of(e).to_owned())
                });

                // 头部
                let (hrect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
                ui.painter().line_segment(
                    [egui::pos2(hrect.left(), hrect.bottom() - 0.5), egui::pos2(hrect.right(), hrect.bottom() - 0.5)],
                    Stroke::new(1.0, p.border),
                );
                if let Some((path, _, _, is_dir, name)) = &meta {
                    let cat = icons::file_cat_index(name, *is_dir);
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(egui::pos2(hrect.left() + 22.0, hrect.center().y), Vec2::splat(16.0)),
                        icons::for_file(name, *is_dir),
                        tokens::CAT_COLORS[cat],
                        1.5,
                    );
                    let _ = path;
                    let clip = Rect::from_min_max(
                        egui::pos2(hrect.left() + 38.0, hrect.top()),
                        egui::pos2(hrect.right() - 46.0, hrect.bottom()),
                    );
                    ui.painter().with_clip_rect(clip).text(
                        egui::pos2(hrect.left() + 38.0, hrect.center().y),
                        egui::Align2::LEFT_CENTER,
                        elide(name, 30),
                        egui::FontId::proportional(12.5),
                        p.text,
                    );
                } else {
                    ui.painter().text(
                        egui::pos2(hrect.left() + 12.0, hrect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "预览",
                        egui::FontId::proportional(12.5),
                        p.text2,
                    );
                }
                let cr = Rect::from_center_size(egui::pos2(hrect.right() - 22.0, hrect.center().y), Vec2::splat(24.0));
                let cresp = ui.interact(cr, ui.id().with("prev_close"), Sense::click());
                if cresp.hovered() {
                    ui.painter().rect_filled(cr, 4.0, p.hover);
                }
                icons::paint(ui.painter(), Rect::from_center_size(cr.center(), Vec2::splat(12.0)), Icon::Close, p.text2, 1.7);
                cresp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "关闭预览"));
                if cresp.clicked() {
                    self.right_open = false;
                    self.config.ui.preview_open = false;
                    config::save(&self.config);
                }

                // 内容
                let body_h = (ui.available_height() - 96.0).max(120.0);
                egui::ScrollArea::vertical()
                    .max_height(body_h)
                    .auto_shrink([false, false])
                    .id_salt("prev_scroll")
                    .show(ui, |ui| {
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.add_space(12.0);
                            ui.vertical(|ui| {
                                ui.set_width((w - 32.0).max(80.0));
                                self.preview_body(ui, p, &meta);
                            });
                        });
                    });

                // 标记区
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new("标记").size(11.0).color(p.text3));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(10.0);
                        if ui
                            .add(egui::Label::new(RichText::new("清除").size(11.0).color(p.text3)).sense(Sense::click()))
                            .clicked()
                        {
                            actions.push(Action::ClearMarks);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let targets = self.mark_targets();
                    let current = meta.as_ref().and_then(|(pth, ..)| self.config.marks.get(pth).copied());
                    for (ci, col) in tokens::MARKS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::click());
                        if current == Some(ci as u8) {
                            ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(2.0, p.text));
                        }
                        ui.painter().circle_filled(r.center(), 9.0, *col);
                        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, format!("标记颜色 {}", ci + 1)));
                        if resp.clicked() && !targets.is_empty() {
                            actions.push(Action::Mark(targets.clone(), Some(ci as u8)));
                        }
                    }
                });
                ui.add_space(6.0);
            });
    }

    fn preview_body(
        &mut self,
        ui: &mut egui::Ui,
        p: &Pal,
        meta: &Option<(String, u64, u64, bool, String)>,
    ) {
        let Some((path, size, mtime, is_dir, name)) = meta else {
            ui.add_space(110.0);
            ui.vertical_centered(|ui| {
                icons::image(ui, Icon::Search, 32.0, p.text3);
                ui.add_space(8.0);
                ui.label(RichText::new("暂无预览").size(13.0).color(p.text3));
                ui.add_space(2.0);
                ui.label(RichText::new("选择一项查看详情").size(11.5).color(p.text3));
            });
            return;
        };
        let _ = name;

        let (tex, text, loading, caption) = match &self.preview {
            Some(pv) => (pv.texture.clone(), pv.text.clone(), pv.loading, pv.caption),
            None => (None, None, false, None),
        };

        if let Some(t) = tex {
            let sz = t.size_vec2();
            let max_w = ui.available_width();
            let max_h = 300.0;
            let scale = (max_w / sz.x).min(max_h / sz.y).min(1.0);
            let target = sz * scale;
            let (area, _) = ui.allocate_exact_size(Vec2::new(max_w, target.y + 2.0), Sense::hover());
            let img_rect = Rect::from_center_size(area.center(), target);
            // 1px 边框让浅色图片不糊在面板底色上
            ui.painter().rect_filled(img_rect.expand(1.0), tokens::size::RADIUS, p.border);
            let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(img_rect));
            cell.add(egui::Image::new(&t).fit_to_exact_size(target));

            ui.add_space(7.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(c) = caption {
                    let (r, _) = ui.allocate_exact_size(
                        Vec2::new(
                            ui.painter()
                                .layout_no_wrap(c.to_owned(), egui::FontId::proportional(10.0), p.brand)
                                .size()
                                .x
                                + 12.0,
                            16.0,
                        ),
                        Sense::hover(),
                    );
                    ui.painter().rect_filled(r, 4.0, p.brand_soft);
                    ui.painter().text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        c,
                        egui::FontId::proportional(10.0),
                        p.brand,
                    );
                }
                ui.label(
                    RichText::new(format!("{:.0} × {:.0}", sz.x, sz.y))
                        .size(10.5)
                        .monospace()
                        .color(p.text3),
                );
            });
        } else if let Some(t) = text {
            let shown: String = t.lines().take(600).collect::<Vec<_>>().join("\n");
            egui::Frame::none()
                .fill(p.bg)
                .stroke(Stroke::new(1.0, p.border))
                .rounding(tokens::size::RADIUS)
                .inner_margin(egui::Margin::same(9.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(shown).size(11.5).monospace().color(p.text2)).wrap(),
                    );
                });
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("{} 行", t.lines().count()))
                    .size(10.5)
                    .monospace()
                    .color(p.text3),
            );
        } else if loading {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.add_space(6.0);
                ui.label(RichText::new("正在读取…").size(12.0).color(p.text3));
            });
        } else {
            ui.add_space(28.0);
            ui.vertical_centered(|ui| {
                icons::image(ui, Icon::Doc, 26.0, p.text3);
                ui.add_space(8.0);
                ui.label(RichText::new("没有可用的预览").size(12.0).color(p.text2));
                ui.add_space(2.0);
                ui.label(RichText::new("该格式没有系统缩略图").size(11.0).color(p.text3));
            });
        }

        ui.add_space(12.0);
        let row = |ui: &mut egui::Ui, k: &str, v: &str, mono: bool| {
            ui.horizontal_top(|ui| {
                ui.add_sized(
                    [46.0, 16.0],
                    egui::Label::new(RichText::new(k).size(11.0).color(p.text3)),
                );
                let mut t = RichText::new(v).size(11.5).color(p.text2);
                if mono {
                    t = t.monospace();
                }
                ui.add(egui::Label::new(t).wrap());
            });
        };
        row(ui, "大小", &if *is_dir { "—".into() } else { fmt_size(*size) }, true);
        row(ui, "修改", &fmt_filetime(*mtime), true);
        row(ui, "路径", path, true);
    }

    // ---------- 中央区 ----------

    fn central(&mut self, ctx: &egui::Context, p: &Pal, actions: &mut Vec<Action>) {
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(p.bg))
            .show(ctx, |ui| {
                if self.bp == Breakpoint::Xs {
                    self.chips_row(ui, p, actions);
                }
                let cols = Cols::layout(ui.available_width(), self.bp);
                self.header_row(ui, p, &cols, actions);
                // 窄屏没有路径列时，行内改两行式承载路径
                let row_h = if cols.show_path {
                    self.config.ui.density.row()
                } else {
                    self.config.ui.density.row().max(44.0)
                };
                if self.results.is_empty() {
                    self.empty_state(ui, p, actions);
                } else {
                    self.table(ui, p, &cols, row_h, actions);
                }
            });
    }

    fn chips_row(&mut self, ui: &mut egui::Ui, p: &Pal, actions: &mut Vec<Action>) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
        {
            let mut chips = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            chips.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(8.0);
                for c in Category::ALL {
                    if widgets::chip(ui, p, c.label(), self.cat == c).clicked() {
                        actions.push(Action::SetCat(c));
                    }
                }
            });
        }
        ui.painter().line_segment(
            [egui::pos2(rect.left(), rect.bottom() - 0.5), egui::pos2(rect.right(), rect.bottom() - 0.5)],
            Stroke::new(1.0, p.border),
        );
    }

    fn header_row(&mut self, ui: &mut egui::Ui, p: &Pal, cols: &Cols, actions: &mut Vec<Action>) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), tokens::size::HEADER), Sense::hover());
        ui.painter().line_segment(
            [egui::pos2(rect.left(), rect.bottom() - 0.5), egui::pos2(rect.right(), rect.bottom() - 0.5)],
            Stroke::new(1.0, p.border),
        );
        let mut x = rect.left();
        let top = rect.top();

        // 全选
        let cbr = Rect::from_center_size(
            egui::pos2(x + cols.check / 2.0, rect.center().y),
            Vec2::splat(15.0),
        );
        let cb = ui.interact(cbr, ui.id().with("all_cb"), Sense::click());
        cb.widget_info(|| WidgetInfo::labeled(WidgetType::Checkbox, true, "全选"));
        let all = !self.results.is_empty() && self.batch.len() >= self.results.len();
        ui.painter().rect_stroke(cbr, 3.0, Stroke::new(1.0, if all { p.brand } else { p.border2 }));
        if all {
            ui.painter().rect_filled(cbr, 3.0, p.brand);
            icons::paint(ui.painter(), cbr.shrink(3.0), Icon::Check, p.on_brand, 2.0);
        }
        if cb.clicked() {
            if all {
                self.batch.clear();
            } else {
                self.batch = self.results.iter().copied().collect();
            }
        }
        x += cols.check;

        let mut col = |ui: &mut egui::Ui, x: f32, w: f32, label: &str, key: SortKey, right: bool| {
            let r = Rect::from_min_size(egui::pos2(x, top), Vec2::new(w, tokens::size::HEADER));
            let resp = ui.interact(r, ui.id().with(("h", label)), Sense::click());
            resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
            if resp.hovered() {
                ui.painter().rect_filled(r.shrink2(Vec2::new(2.0, 4.0)), 4.0, p.hover);
            }
            let active = self.sort_key == key;
            let color = if active { p.text } else { p.text2 };
            let g = ui
                .painter()
                .layout_no_wrap(label.to_owned(), egui::FontId::proportional(12.0), color);
            let tw = g.size().x;
            let ty = r.center().y - g.size().y / 2.0;
            let tx = if right { r.right() - 8.0 - tw } else { r.left() + 8.0 };
            ui.painter().galley(egui::pos2(tx, ty), g, color);
            if active {
                let cx = if right { tx - 11.0 } else { tx + tw + 11.0 };
                let cy = r.center().y;
                let pts = if self.sort_desc {
                    vec![
                        egui::pos2(cx - 3.5, cy - 2.0),
                        egui::pos2(cx + 3.5, cy - 2.0),
                        egui::pos2(cx, cy + 2.6),
                    ]
                } else {
                    vec![
                        egui::pos2(cx - 3.5, cy + 2.0),
                        egui::pos2(cx + 3.5, cy + 2.0),
                        egui::pos2(cx, cy - 2.6),
                    ]
                };
                ui.painter().add(egui::Shape::convex_polygon(pts, p.brand, Stroke::NONE));
            }
            if resp.clicked() {
                self.toggle_sort(key);
            }
        };

        col(ui, x, cols.name, "文件名", SortKey::Name, false);
        x += cols.name;
        if cols.show_path {
            col(ui, x, cols.path, "文件路径", SortKey::Path, false);
            x += cols.path;
        }
        col(ui, x, cols.size, "文件大小", SortKey::Size, true);
        x += cols.size;
        if cols.show_time {
            col(ui, x, cols.time, "修改时间", SortKey::Mtime, true);
        }
        let _ = actions;
    }

    fn toggle_sort(&mut self, k: SortKey) {
        if self.sort_key == k {
            self.sort_desc = !self.sort_desc;
        } else {
            self.sort_key = k;
            self.sort_desc = false;
        }
        self.needs_search = true;
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, p: &Pal, actions: &mut Vec<Action>) {
        let st = self.handle.status.get();
        let walk_offered = !indexer::allow_walk();
        let (icon, title, body, action) = if !st.ready {
            (
                Icon::Refresh,
                "正在建立索引".to_string(),
                if let Some(e) = &st.error {
                    e.clone()
                } else {
                    format!("已扫描 {}/{} 个卷，完成后可全盘秒搜", st.volumes_done, st.volumes_total)
                },
                None,
            )
        } else if let Some(e) = st.error.clone() {
            (
                Icon::Shield,
                "部分卷未能索引".to_string(),
                e,
                if walk_offered { Some(("开启遍历模式并重建", 2u8)) } else { None },
            )
        } else if self.query.trim().is_empty() && self.cat == Category::All {
            (
                Icon::Folder,
                "索引里还没有文件".to_string(),
                "点击「建立索引」重新扫描本地卷；以管理员运行可启用 MFT 直读".to_string(),
                Some(("建立索引", 0u8)),
            )
        } else {
            (
                Icon::Search,
                format!("没有找到「{}」", self.query.trim()),
                "试试空格分隔多个关键词，或用反斜杠指定路径片段".to_string(),
                Some(("清除筛选", 1u8)),
            )
        };

        ui.add_space((ui.available_height() * 0.18).max(20.0));
        ui.vertical_centered(|ui| {
            icons::image(ui, icon, 40.0, p.text3);
            ui.add_space(10.0);
            ui.label(RichText::new(title).size(15.0).color(p.text));
            ui.add_space(4.0);
            ui.label(RichText::new(body).size(12.0).color(p.text3));
            if let Some((label, kind)) = action {
                ui.add_space(14.0);
                if widgets::button(ui, p, label, kind != 1, if kind == 2 { 148.0 } else { 88.0 }).clicked() {
                    match kind {
                        0 => {
                            indexer::request_refresh(&self.handle);
                            actions.push(Action::Toast("正在重建索引…".into(), true));
                        }
                        2 => {
                            self.config.allow_walk = true;
                            config::save(&self.config);
                            indexer::set_allow_walk(true);
                            indexer::request_refresh(&self.handle);
                            actions.push(Action::Toast("已开启遍历模式，正在重建索引…".into(), true));
                        }
                        _ => {
                            self.query.clear();
                            self.cat = Category::All;
                            self.needs_search = true;
                        }
                    }
                }
            }
        });
    }

    fn table(&mut self, ui: &mut egui::Ui, p: &Pal, cols: &Cols, row_h: f32, actions: &mut Vec<Action>) {
        let total = self.results.len();
        let index = self.handle.index.clone();
        let results = self.results.clone();
        let mut sel = self.selected;
        let batch = std::mem::take(&mut self.batch);
        let marks = self.config.marks.clone();

        {
            let ix = index.read().unwrap();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .id_salt("results_scroll")
                .show_rows(ui, row_h, total, |ui, range| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for i in range {
                        let Some(&ei) = results.get(i) else { continue };
                        let Some(e) = ix.entries.get(ei as usize) else { continue };
                        let name = ix.name_of(e);
                        let is_dir = e.is_dir();
                        let size = e.size;
                        let mtime = e.mtime;
                        let parent = ix.parent_path(ei);
                        let full = {
                            let p = parent.trim_end_matches('\\');
                            if p.is_empty() { name.to_string() } else { format!("{p}\\{name}") }
                        };

                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), row_h),
                            Sense::click(),
                        );
                        let painter = ui.painter().clone();
                        let ptr_in = ui.ctx().pointer_hover_pos().map_or(false, |q| rect.contains(q));
                        let is_sel = sel == Some(i as u32);

                        // 路径列是尾部优先的压缩形式，鼠标停留时给出完整路径
                        if !parent.is_empty() {
                            resp.clone().on_hover_text(parent.clone());
                        }

                        if is_sel {
                            painter.rect_filled(rect, 0.0, p.brand_soft);
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, Vec2::new(2.0, row_h)),
                                0.0,
                                p.brand,
                            );
                        } else if ptr_in {
                            painter.rect_filled(rect, 0.0, p.hover);
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, Vec2::new(2.0, row_h)),
                                0.0,
                                p.border2,
                            );
                        }

                        let cy = rect.center().y;
                        let mut x = rect.left();
                        let mut child_hit = false;

                        // 勾选
                        let cbr = Rect::from_center_size(egui::pos2(x + cols.check / 2.0, cy), Vec2::splat(15.0));
                        let cb = ui.interact(cbr, resp.id.with("cb"), Sense::click());
                        let checked = batch.contains(&ei);
                        painter.rect_filled(
                            cbr,
                            3.0,
                            if checked { p.brand } else { Color32::TRANSPARENT },
                        );
                        painter.rect_stroke(
                            cbr,
                            3.0,
                            Stroke::new(1.0, if checked { p.brand } else { p.border2 }),
                        );
                        if checked {
                            icons::paint(&painter, cbr.shrink(3.0), Icon::Check, p.on_brand, 2.0);
                        }
                        cb.widget_info(|| {
                            WidgetInfo::labeled(WidgetType::Checkbox, true, if checked { "已选择" } else { "选择" })
                        });
                        if cb.clicked() {
                            child_hit = true;
                            actions.push(Action::ToggleBatch(ei));
                        }
                        x += cols.check;

                        // 名称
                        let name_rect = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(cols.name, row_h));
                        let cat = icons::file_cat_index(name, is_dir);
                        {
                            let cell_rect = name_rect.shrink2(Vec2::new(8.0, 0.0));
                            let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(cell_rect));
                            if cols.show_path {
                                cell.horizontal_centered(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    icons::image(ui, icons::for_file(name, is_dir), 16.0, tokens::CAT_COLORS[cat]);
                                    let txt = RichText::new(name).size(13.0).color(p.text);
                                    let lbl = if is_sel {
                                        egui::Label::new(RichText::new(name).size(13.0).color(p.text).strong())
                                    } else {
                                        egui::Label::new(txt)
                                    };
                                    ui.add(lbl.truncate().selectable(false));
                                });
                            } else {
                                // 两行式：每行给固定高度，整体垂直居中，避免行距塌陷
                                let line_h = 17.0;
                                let lines = if parent.is_empty() { 1.0 } else { 2.0 };
                                let cw = cell_rect.width();
                                cell.spacing_mut().item_spacing.y = 0.0;
                                cell.add_space(((row_h - line_h * lines) * 0.5).max(0.0));
                                cell.allocate_ui_with_layout(
                                    Vec2::new(cw, line_h),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        icons::image(ui, icons::for_file(name, is_dir), 14.0, tokens::CAT_COLORS[cat]);
                                        ui.add(
                                            egui::Label::new(RichText::new(name).size(12.5).color(p.text))
                                                .truncate()
                                                .selectable(false),
                                        );
                                    },
                                );
                                if !parent.is_empty() {
                                    cell.allocate_ui_with_layout(
                                        Vec2::new(cw, line_h),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            // 与宽屏一致：盘符开头的完整路径，截尾
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&parent).size(10.5).monospace().color(p.text3),
                                                )
                                                .truncate()
                                                .selectable(false),
                                            );
                                        },
                                    );
                                }
                            }
                        }
                        x += cols.name;

                        // 路径
                        if cols.show_path {
                            let pr = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(cols.path, row_h));
                            path_cell(ui, pr.shrink2(Vec2::new(8.0, 0.0)), &parent, 11.0, p.text2);
                            x += cols.path;
                        }

                        // 大小
                        let sr = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(cols.size, row_h));
                        text_cell(
                            ui,
                            sr.shrink2(Vec2::new(8.0, 0.0)),
                            &if is_dir { "—".to_string() } else { fmt_size(size) },
                            12.0,
                            p.text2,
                            true,
                            true,
                        );
                        x += cols.size;

                        // 时间
                        if cols.show_time {
                            let tr = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(cols.time, row_h));
                            text_cell(ui, tr.shrink2(Vec2::new(8.0, 0.0)), &fmt_when(mtime), 12.0, p.text2, true, true);
                            x += cols.time;
                        }

                        // 标记点
                        if cols.show_mark {
                            if let Some(&c) = marks.get(&full) {
                                let col = tokens::MARKS.get(c as usize % tokens::MARKS.len()).copied().unwrap_or(p.brand);
                                painter.circle_filled(egui::pos2(x + cols.mark * 0.5, cy), 4.0, col);
                            }
                        }

                        // 悬浮操作
                        if ptr_in {
                            // 五个动作：打开 / 定位 / 复制 / 标记 / 删除
                            let bw = 5.0 * 24.0 + 6.0;
                            let bx = Rect::from_center_size(
                                egui::pos2(rect.right() - 14.0 - bw / 2.0, cy),
                                Vec2::new(bw, 26.0),
                            );
                            painter.rect_filled(bx, 6.0, p.raised);
                            painter.rect_stroke(bx, 6.0, Stroke::new(1.0, p.border2));
                            let items = [
                                (Icon::External, "打开"),
                                (Icon::Folder, "打开所在位置"),
                                (Icon::Copy, "复制路径"),
                                (Icon::Tag, "标记"),
                                (Icon::Trash, "删除"),
                            ];
                            for (k, (ic, tip)) in items.iter().enumerate() {
                                let br = Rect::from_center_size(
                                    egui::pos2(bx.left() + 5.0 + 12.0 + k as f32 * 24.0, cy),
                                    Vec2::splat(24.0),
                                );
                                let bresp = ui.interact(br, resp.id.with(("act", k)), Sense::click());
                                if bresp.hovered() {
                                    painter.rect_filled(br, 4.0, p.hover);
                                }
                                icons::paint(
                                    &painter,
                                    Rect::from_center_size(br.center(), Vec2::splat(14.0)),
                                    *ic,
                                    if bresp.hovered() { p.text } else { p.text2 },
                                    1.5,
                                );
                                bresp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, *tip));
                                if bresp.clicked() {
                                    child_hit = true;
                                    match k {
                                        0 => actions.push(Action::Open(ei)),
                                        1 => actions.push(Action::OpenLoc(ei)),
                                        2 => actions.push(Action::Copy(full.clone())),
                                        3 => actions.push(Action::Mark(vec![ei], Some(0))),
                                        _ => actions.push(Action::Delete(vec![ei])),
                                    }
                                }
                            }
                        }

                        if !child_hit {
                            if resp.double_clicked() {
                                sel = Some(i as u32);
                                actions.push(Action::Open(ei));
                            } else if resp.clicked() {
                                sel = Some(i as u32);
                            }
                        }
                        resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, name));
                        resp.context_menu(|ui| {
                            let targets = {
                                let mut v: Vec<u32> = batch.iter().copied().collect();
                                if !v.contains(&ei) {
                                    v.push(ei);
                                }
                                v
                            };
                            if ui.button("打开").clicked() {
                                actions.push(Action::Open(ei));
                                sel = Some(i as u32);
                                ui.close_menu();
                            }
                            if ui.button("打开所在位置").clicked() {
                                actions.push(Action::OpenLoc(ei));
                                ui.close_menu();
                            }
                            if ui.button("复制完整路径").clicked() {
                                actions.push(Action::Copy(full.clone()));
                                ui.close_menu();
                            }
                            ui.separator();
                            ui.menu_button("标记为", |ui| {
                                for (ci, c) in tokens::MARKS.iter().enumerate() {
                                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
                                    ui.painter().circle_filled(r.center(), 7.0, *c);
                                    if resp.clicked() {
                                        actions.push(Action::Mark(targets.clone(), Some(ci as u8)));
                                        ui.close_menu();
                                    }
                                }
                                if ui.button("移除标记").clicked() {
                                    actions.push(Action::Mark(targets.clone(), None));
                                    ui.close_menu();
                                }
                            });
                            if ui.button("清除全部标记").clicked() {
                                actions.push(Action::ClearMarks);
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("删除（移入回收站）").clicked() {
                                actions.push(Action::Delete(targets.clone()));
                                ui.close_menu();
                            }
                        });
                    }
                });
        }

        self.selected = sel;
        self.batch = batch;
    }

    // ---------- 设置 ----------

    fn settings_window(&mut self, ctx: &egui::Context, p: &Pal) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut do_apply = false;
        let mut do_close = false;
        // 对话框随窗口收缩，避免在小窗里被裁掉（标题与按钮必须始终可见）
        let screen = ctx.screen_rect();
        let dlg_w = (screen.width() - 48.0).clamp(420.0, 600.0);
        let dlg_h = (screen.height() - 48.0).clamp(320.0, 640.0);
        // 模态遮罩：把背后的表格压暗并吃掉点击，否则弹窗与背景粘连
        egui::Area::new(egui::Id::new("modal_mask"))
            .order(egui::Order::Middle)
            .fixed_pos(screen.min)
            .interactable(true)
            .show(ctx, |ui| {
                let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click_and_drag());
                ui.painter()
                    .rect_filled(r, 0.0, Color32::from_black_alpha(if p.dark { 120 } else { 55 }));
            });

        let frame = egui::Frame::none()
            .fill(p.raised)
            .stroke(Stroke::new(1.0, p.border2))
            .rounding(egui::Rounding::same(10.0))
            .shadow(egui::epaint::Shadow {
                offset: egui::vec2(0.0, 12.0),
                blur: 32.0,
                spread: 0.0,
                color: Color32::from_black_alpha(if p.dark { 120 } else { 40 }),
            });

        egui::Window::new("系统设置")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .fixed_size([dlg_w, dlg_h])
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(frame)
            .show(ctx, |ui| {
                // 自绘标题行：沿用系统标题栏会带一层灰底，和整体色板冲突
                let (hr, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), 44.0),
                    Sense::hover(),
                );
                ui.painter().text(
                    egui::pos2(hr.left() + 16.0, hr.center().y),
                    egui::Align2::LEFT_CENTER,
                    "系统设置",
                    egui::FontId::proportional(14.0),
                    p.text,
                );
                let cr = Rect::from_center_size(
                    egui::pos2(hr.right() - 22.0, hr.center().y),
                    Vec2::splat(24.0),
                );
                let cresp = ui.interact(cr, ui.id().with("dlg_x"), Sense::click());
                if cresp.hovered() {
                    ui.painter().rect_filled(cr, 4.0, p.hover);
                }
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(cr.center(), Vec2::splat(12.0)),
                    Icon::Close,
                    p.text2,
                    1.7,
                );
                cresp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, "关闭设置"));
                if cresp.clicked() {
                    do_close = true;
                }
                ui.painter().line_segment(
                    [
                        egui::pos2(hr.left(), hr.bottom() - 0.5),
                        egui::pos2(hr.right(), hr.bottom() - 0.5),
                    ],
                    Stroke::new(1.0, p.border),
                );

                let footer_h = 52.0;
                let body_h = (ui.available_height() - footer_h).max(160.0);
                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(16.0, 4.0))
                    .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(body_h)
                    .auto_shrink([false, false])
                    .id_salt("settings_scroll")
                    .show(ui, |ui| {
                        widgets::section(ui, p, "开机启动设置");
                        ui.checkbox(&mut self.draft.autostart, "开机时自动启动");
                        ui.label(
                            RichText::new("随系统启动后索引常驻内存，唤起即可秒搜，无需等待")
                                .size(11.0)
                                .color(p.text3),
                        );

                        ui.add_space(14.0);
                        widgets::section(ui, p, "界面");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("主题").size(12.0).color(p.text2));
                            ui.add_space(8.0);
                            for t in ThemePref::ALL {
                                if widgets::chip(ui, p, t.label(), self.draft.theme == t).clicked() {
                                    self.draft.theme = t;
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("行高").size(12.0).color(p.text2));
                            ui.add_space(8.0);
                            for d in Density::ALL {
                                if widgets::chip(ui, p, d.label(), self.draft.density == d).clicked() {
                                    self.draft.density = d;
                                }
                            }
                        });

                        ui.add_space(14.0);
                        widgets::section(ui, p, "排除目录");
                        ui.label(
                            RichText::new("排除后的目录不进入索引，可显著减少条目数与内存占用")
                                .size(11.0)
                                .color(p.text3),
                        );
                        ui.horizontal(|ui| {
                            if widgets::button(ui, p, "添加目录", false, 84.0).clicked() {
                                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                    self.draft.excluded.push(dir.to_string_lossy().into_owned());
                                }
                            }
                            if widgets::button(ui, p, "移除选中", false, 84.0).clicked() {
                                if let Some(i) = self.draft.sel_excl {
                                    if i < self.draft.excluded.len() {
                                        self.draft.excluded.remove(i);
                                        self.draft.sel_excl = None;
                                    }
                                }
                            }
                        });
                        egui::Frame::none()
                            .fill(p.bg)
                            .stroke(Stroke::new(1.0, p.border))
                            .rounding(tokens::size::RADIUS)
                            .inner_margin(egui::Margin::same(6.0))
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("excl_list")
                                    .max_height(120.0)
                                    .show(ui, |ui| {
                                        if self.draft.excluded.is_empty() {
                                            ui.label(RichText::new("暂无排除目录").size(11.5).color(p.text3));
                                        }
                                        for (i, d) in self.draft.excluded.iter().enumerate() {
                                            let sel = self.draft.sel_excl == Some(i);
                                            let txt = RichText::new(d)
                                                .size(11.5)
                                                .color(if sel { p.brand_text } else { p.text2 });
                                            let r = ui.add(
                                                egui::Label::new(txt).sense(Sense::click()).truncate(),
                                            );
                                            if sel {
                                                ui.painter().rect_filled(r.rect, 3.0, p.brand_soft);
                                            }
                                            if r.clicked() {
                                                self.draft.sel_excl = Some(i);
                                            }
                                        }
                                    });
                            });

                        ui.add_space(14.0);
                        widgets::section(ui, p, "热键");
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("唤起主界面的全局快捷键").size(12.0).color(p.text2));
                            ui.add_space(8.0);
                            ui.add_sized(
                                [140.0, 26.0],
                                egui::TextEdit::singleline(&mut self.draft.hotkey)
                                    .font(egui::TextStyle::Monospace),
                            );
                        });
                        ui.label(
                            RichText::new("格式如 Alt + ` / Ctrl + Alt + F1，保存后立即生效")
                                .size(11.0)
                                .color(p.text3),
                        );

                        ui.add_space(14.0);
                        widgets::section(ui, p, "关于");
                        ui.label(
                            RichText::new(format!("迅搜 v{VERSION} · Rust / egui"))
                                .size(12.0)
                                .color(p.text2),
                        );
                        ui.label(
                            RichText::new("索引缓存在 %LOCALAPPDATA%\\xunsou，不会写入被索引的磁盘")
                                .size(11.0)
                                .color(p.text3),
                        );
                    });

                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, p, "确定", true, 72.0).clicked() {
                        do_apply = true;
                        do_close = true;
                    }
                    let fr = ui.max_rect();
                    ui.painter().line_segment(
                        [
                            egui::pos2(fr.left(), fr.top() - 6.0),
                            egui::pos2(fr.right(), fr.top() - 6.0),
                        ],
                        Stroke::new(1.0, p.border),
                    );
                    ui.add_space(8.0);
                    if widgets::button(ui, p, "取消", false, 72.0).clicked() {
                        do_close = true;
                    }
                });
                    });
            });
        if !open {
            do_close = true;
        }
        if do_apply {
            self.apply_settings();
        }
        if do_close {
            self.settings_open = false;
        }
    }

    // ---------- Toast ----------

    fn toast_overlay(&mut self, ctx: &egui::Context, p: &Pal) {
        let Some((msg, at, ok)) = self.toast.clone() else { return };
        if at.elapsed() > Duration::from_millis(2400) {
            self.toast = None;
            return;
        }
        egui::Area::new("toast".into())
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -44.0))
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(p.raised)
                    .stroke(Stroke::new(1.0, p.border2))
                    .rounding(8.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            icons::image(ui, if ok { Icon::Check } else { Icon::Close }, 14.0, if ok { p.ok } else { p.danger });
                            ui.add_space(2.0);
                            ui.label(RichText::new(&msg).size(12.5).color(p.text));
                        });
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(80));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_frame(ctx);
    }
}

/// 单元格文本（可选等宽 / 右对齐 / 单行省略）。
/// 路径单元格：从盘符开始的完整父路径，宽度不足时截尾。
///
/// 参考样式的要点是**开头恒为盘符**（`G://game//DNF旧版解包//...`），
/// 识别一个结果属于哪个盘 / 哪个顶层目录只需要看行首；末端被截掉的
/// 部分交给悬停提示补全。
fn path_cell(ui: &mut egui::Ui, rect: Rect, path: &str, size_px: f32, color: Color32) {
    if path.is_empty() {
        return;
    }
    let mut cell = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    cell.add(
        egui::Label::new(RichText::new(path).size(size_px).monospace().color(color))
            .truncate()
            .selectable(false),
    );
}

fn text_cell(ui: &mut egui::Ui, rect: Rect, text: &str, size_px: f32, color: Color32, mono: bool, right: bool) {
    let lay = if right {
        egui::Layout::right_to_left(egui::Align::Center)
    } else {
        egui::Layout::left_to_right(egui::Align::Center)
    };
    // 用 new_child：子 UI 不影响父级游标，避免行高被回退成内容高度
    let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(lay));
    let mut t = RichText::new(text).size(size_px).color(color);
    if mono {
        t = t.monospace();
    }
    cell.add(egui::Label::new(t).truncate().selectable(false));
}

// ============================ 工具函数 ============================

pub fn fmt_size(b: u64) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 * 1024 {
        // 与原版一致：1GB 以下都用千分位 KB
        format!("{} KB", group(b.div_ceil(1024)))
    } else if b < 1024_u64 * 1024 * 1024 * 1024 {
        format!("{:.2} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!("{:.2} TB", b as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0))
    }
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + 4);
    let bytes = s.as_bytes();
    for (i, &c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c as char);
    }
    out
}

pub fn fmt_filetime(ft: u64) -> String {
    if ft == 0 {
        return "-".into();
    }
    // FILETIME → Unix 秒
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let (y, mo, d, h, mi) = civil_from_unix(secs);
    format!("{y:04}/{mo:02}/{d:02} {h:02}:{mi:02}")
}

/// 列表用相对时间（本地时区）。
fn fmt_when(ft: u64) -> String {
    if ft == 0 {
        return "—".into();
    }
    let local = win::filetime_to_local(ft);
    let secs = (local / 10_000_000) as i64 - 11_644_473_600;
    let now = win::filetime_to_local(win::now_filetime());
    let now_secs = (now / 10_000_000) as i64 - 11_644_473_600;
    let (y, mo, d, h, mi) = civil_from_unix(secs);
    let days = secs.div_euclid(86_400);
    let today = now_secs.div_euclid(86_400);
    if days == today {
        format!("今天 {h:02}:{mi:02}")
    } else if days == today - 1 {
        format!("昨天 {h:02}:{mi:02}")
    } else if days == today - 2 {
        format!("前天 {h:02}:{mi:02}")
    } else if y == civil_from_unix(now_secs).0 {
        format!("{mo}月{d}日")
    } else {
        format!("{y:04}/{mo:02}/{d:02}")
    }
}

fn fmt_count(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn elide(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Unix 秒 → (年,月,日,时,分)。Howard Hinnant 的 civil_from_days 算法。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi) = ((rem / 3600) as u32, ((rem % 3600) / 60) as u32);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi)
}

#[allow(dead_code)]
fn shorten(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_owned()
    } else {
        let head: String = s.chars().take(max_chars / 2).collect();
        let tail: String = s.chars().rev().take(max_chars - max_chars / 2 - 1).collect();
        format!("{head}…{}", tail.chars().rev().collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_fmt() {
        assert_eq!(fmt_size(512), "512 B");
        assert_eq!(fmt_size(1024), "1 KB");
        assert_eq!(fmt_size(27_502 * 1024), "27,502 KB");
    }

    #[test]
    fn time_fmt() {
        // 2026-01-02 03:04 UTC = 1767323040
        let ft = (1767323040u64 + 11_644_473_600) * 10_000_000;
        assert_eq!(fmt_filetime(ft), "2026/01/02 03:04");
    }
}

/// 无头 UI 测试（egui-kittest）：不需要窗口/显卡即可驱动真实 UI 与交互。
#[cfg(test)]
mod headless {
    use super::*;
    use crate::config::Config;
    use crate::indexer::{IndexHandle, Status, StatusHandle};
    use crate::model::{EntryRecord, Index, FLAG_DIR, NO_PARENT};
    use egui_kittest::{Harness, kittest::Queryable};
    use std::sync::{Arc, RwLock};

    fn rec(name: &str, frn: u64, parent_idx: u32, parent_frn: u64, dir: bool, size: u64) -> EntryRecord {
        // mtime = 2022-01-01 00:00 UTC 的 FILETIME
        const SAMPLE_FILETIME: u64 = (1_325_376_000u64 + 11_644_473_600) * 10_000_000;
        EntryRecord {
            frn,
            parent_frn,
            parent_idx,
            name: name.into(),
            size,
            mtime: SAMPLE_FILETIME,
            flags: if dir { FLAG_DIR } else { 0 },
            is_root: false,
        }
    }

    /// 构造带样例索引的无头 App（不启动真实索引/热键线程）。
    fn sample_app() -> (egui::Context, App) {
        let ctx = egui::Context::default();
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
        let d1 = ix.add_entry(rec("Photos", 10, root, 5, true, 0));
        ix.add_entry(rec("sunset.jpg", 11, d1, 10, false, 2048));
        ix.add_entry(rec("holiday.mp4", 12, d1, 10, false, 999_999_999));
        let d2 = ix.add_entry(rec("Docs", 20, root, 5, true, 0));
        ix.add_entry(rec("readme.txt", 21, d2, 20, false, 1024));
        ix.recompute_exclusions(&[]);

        let status = StatusHandle::new(
            ctx.clone(),
            Status {
                phase: "就绪".into(),
                ready: true,
                volumes_done: 1,
                volumes_total: 1,
                error: None,
                needs_rescan: false,
                mft_mode: false,
                started: std::time::Instant::now(),
            },
        );
        let (excl_tx, _) = std::sync::mpsc::channel();
        let (refresh_tx, _) = std::sync::mpsc::channel();
        let handle = IndexHandle { index: Arc::new(RwLock::new(ix)), status, excl_tx, refresh_tx };
        let app = App::with_parts(ctx.clone(), Config::default(), handle);
        (ctx, app)
    }

    fn harness(app: App) -> Harness<'static, App> {
        Harness::builder()
            .with_size(egui::vec2(1100.0, 720.0))
            .build_state(|ctx, app: &mut App| app.update_frame(ctx), app)
    }

    /// 点击/输入在帧中途生效，需要再跑一帧让 ensure_search 等在帧首应用新状态。
    fn settle(h: &mut Harness<'static, App>) {
        h.run();
        h.run();
    }

    #[test]
    fn renders_main_ui_and_all_rows() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        // 存在性断言用 query_all（同名节点可能出现在文件名列与路径列）
        let exists = |h: &Harness<'static, App>, s: &str| h.query_all_by_label_contains(s).count() > 0;
        // 状态栏
        assert!(exists(&h, "总文件"), "缺少总文件数");
        // 分类栏按钮
        for cat in ["全部", "图片", "文档", "视频", "音频", "手机", "目录"] {
            assert!(exists(&h, cat), "缺少分类按钮 {cat}");
        }
        // 表头
        for col in ["文件名", "文件路径", "文件大小", "修改时间"] {
            assert!(exists(&h, col), "缺少表头 {col}");
        }
        // 空查询 → 全部数据行可见
        assert!(exists(&h, "sunset.jpg"));
        assert!(exists(&h, "holiday.mp4"));
        assert!(exists(&h, "readme.txt"));
        // 预览占位
        assert!(exists(&h, "暂无预览"));
    }

    #[test]
    fn category_filter_via_click() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        let exists = |h: &Harness<'static, App>, s: &str| h.query_all_by_label_contains(s).count() > 0;
        // 点击“视频”分类
        h.get_by_label_contains("视频").click();
        settle(&mut h);
        assert!(h.query_all_by_label_contains("sunset.jpg").count() == 0, "图片不应出现在视频分类");
        assert!(exists(&h, "holiday.mp4"), "视频应出现");
        // 切回全部
        h.get_by_label_contains("全部").click();
        settle(&mut h);
        assert!(exists(&h, "sunset.jpg"));
    }

    #[test]
    fn search_by_typing() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        // 通过无障碍树按角色定位搜索框（egui TextEdit → TextInput）
        use egui_kittest::kittest::By;
        let node = h.get(By::new().role(egui::accesskit::Role::TextInput));
        node.click();
        node.type_text("sunset");
        settle(&mut h);
        assert!(
            h.query_all_by_label_contains("sunset.jpg").count() > 0,
            "输入后应命中 sunset.jpg"
        );
        assert!(
            h.query_all_by_label_contains("readme.txt").count() == 0,
            "不匹配项应被过滤"
        );
    }

    #[test]
    fn sort_by_size_header_click() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        assert_eq!(h.state().sort_key, SortKey::Name);
        h.get_by_label_contains("文件大小").click();
        settle(&mut h);
        assert_eq!(h.state().sort_key, SortKey::Size);
        assert!(!h.state().sort_desc, "首次点击应为升序");
        // 升序：目录(0 字节)在前，最小的文件 readme.txt(1KB) 排在文件首位
        {
            let ix = h.state().handle.index.read().unwrap();
            let first_file = h
                .state()
                .results
                .iter()
                .map(|&i| &ix.entries[i as usize])
                .find(|e| !e.is_dir())
                .unwrap()
                .clone();
            assert_eq!(ix.name_of(&first_file), "readme.txt");
        }
        // 再点一次 → 降序：最大的 holiday.mp4 在最前
        h.get_by_label_contains("文件大小").click();
        settle(&mut h);
        assert!(h.state().sort_desc);
        let first = h.state().results[0];
        let name = h.state().handle.index.read().unwrap().entries[first as usize].clone();
        assert_eq!(h.state().handle.index.read().unwrap().name_of(&name), "holiday.mp4");
    }

    #[test]
    fn select_row_updates_selection_state() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        // 名称列与路径列都可能包含 "readme.txt"，取树序第一个（名称列）
        let mut nodes: Vec<_> = h.query_all_by_label_contains("readme.txt").collect();
        assert!(!nodes.is_empty());
        nodes.swap_remove(0).click();
        settle(&mut h);
        assert!(h.state().selected.is_some(), "点击行后应选中");
    }

    #[test]
    fn settings_dialog_open_cancel() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        h.get_by_label("设置").click();
        h.run();
        let exists = |h: &Harness<'static, App>, s: &str| h.query_all_by_label_contains(s).count() > 0;
        assert!(exists(&h, "开机启动设置"), "设置窗口应打开");
        assert!(exists(&h, "排除目录"));
        assert!(h.query_by_label("确定").is_some());
        h.get_by_label("取消").click();
        settle(&mut h);
        assert!(h.query_all_by_label_contains("开机启动设置").count() == 0, "取消后窗口应关闭");
    }

    /// 像素级快照（首次运行需 UPDATE_SNAPSHOTS=1 生成基线）。
    #[test]
    fn snapshot_main_window() {
        let (_ctx, app) = sample_app();
        let mut h = harness(app);
        h.run();
        h.wgpu_snapshot("main");
    }
}

/// 临时 UI 探针：用内存索引渲染真实界面并输出像素快照，仅用于人工核对布局。
#[cfg(test)]
mod ui_probe {
    use super::*;
    use crate::config::Config;
    use crate::indexer::{IndexHandle, Status, StatusHandle};
    use crate::model::{EntryRecord, Index, FLAG_DIR, NO_PARENT};
    use crate::ui::tokens::ThemePref;
    use egui_kittest::{kittest::Queryable, Harness};
    use std::sync::{Arc, RwLock};

    const T0: u64 = (1_760_000_000u64 + 11_644_473_600) * 10_000_000;

    fn rec(
        name: &str,
        frn: u64,
        parent_idx: u32,
        parent_frn: u64,
        dir: bool,
        size: u64,
        age_days: u64,
    ) -> EntryRecord {
        EntryRecord {
            frn,
            parent_frn,
            parent_idx,
            name: name.into(),
            size,
            mtime: T0.saturating_sub(age_days * 864_000_000_000),
            flags: if dir { FLAG_DIR } else { 0 },
            is_root: false,
        }
    }

    /// 造一张「日落」照片：天空渐变 + 太阳 + 地面，用来判断预览的缩放与配色是否正常。
    fn make_photo(path: &std::path::Path) {
        let (w, h) = (960u32, 600u32);
        let mut img = image::RgbaImage::new(w, h);
        let (sx, sy, sr) = (w as f32 * 0.62, h as f32 * 0.46, h as f32 * 0.17);
        let lerp = |a: f32, b: f32, k: f32| a + (b - a) * k.clamp(0.0, 1.0);
        for y in 0..h {
            for x in 0..w {
                let t = y as f32 / h as f32;
                let (mut r, mut g, mut b) = if t < 0.55 {
                    let k = t / 0.55;
                    (lerp(24.0, 238.0, k), lerp(44.0, 150.0, k), lerp(82.0, 104.0, k))
                } else {
                    let k = (t - 0.55) / 0.45;
                    (lerp(46.0, 16.0, k), lerp(36.0, 20.0, k), lerp(44.0, 28.0, k))
                };
                let d = ((x as f32 - sx).powi(2) + (y as f32 - sy).powi(2)).sqrt();
                if d < sr {
                    let k = 1.0 - d / sr;
                    r = lerp(r, 255.0, k * 0.95);
                    g = lerp(g, 226.0, k * 0.95);
                    b = lerp(b, 152.0, k * 0.95);
                }
                img.put_pixel(x, y, image::Rgba([r as u8, g as u8, b as u8, 255]));
            }
        }
        let _ = img.save(path);
    }

    fn build(theme: ThemePref) -> (egui::Context, App) {
        // 预览要读真实文件，所以索引根直接指向临时目录——这样 path_of 拼出来的
        // 就是真实绝对路径，顺带验证「路径列显示完整路径」而不是只有盘符。
        let base = std::env::temp_dir().join("xunsou_preview_probe");
        let photo_dir = base.join("Photos");
        let _ = std::fs::create_dir_all(&photo_dir);
        let photo = photo_dir.join("sunset_2026.png");
        make_photo(&photo);
        let notes = base.join("readme.txt");
        // 逐行拼装，避免长字符串里的换行转义被工具链改写
        let body = [
            "# 迅搜索引说明",
            "",
            "MFT 直读建立名称、路径、大小与时间索引，USN 日志维持增量。",
            "",
            "- 冷启动：顺序读一次 $MFT",
            "- 常态：只处理变更",
            "- 查询：前缀收窄，再做 3-gram 交集",
        ]
        .join(&char::from_u32(10).unwrap().to_string());
        let _ = std::fs::write(&notes, body);

        let ctx = egui::Context::default();
        let mut ix = Index::default();
        let base_s = base.to_string_lossy().into_owned();
        let root = ix.add_entry(EntryRecord {
            frn: 5,
            parent_frn: 0,
            parent_idx: NO_PARENT,
            name: base_s.clone(),
            size: 0,
            mtime: T0,
            flags: FLAG_DIR,
            is_root: true,
        });
        let photos = ix.add_entry(rec("Photos", 10, root, 5, true, 0, 3));
        ix.add_entry(rec("sunset_2026.png", 11, photos, 10, false, 1_887_436, 0));
        ix.add_entry(rec("holiday.mp4", 12, photos, 10, false, 999_999_999, 1));
        let docs = ix.add_entry(rec("Docs", 20, root, 5, true, 0, 4));
        // 真实文本文件直接挂在根下，与 notes 落盘位置一致
        ix.add_entry(rec("readme.txt", 21, root, 5, false, 1024, 2));
        ix.add_entry(rec("季度经营分析_2026年第四季度_最终版_v3.pptx", 22, docs, 20, false, 24_500_000, 0));
        let work = ix.add_entry(rec("工作", 30, root, 5, true, 0, 5));
        ix.add_entry(rec("产品需求文档_v3.2.docx", 31, work, 30, false, 892_400, 0));
        ix.add_entry(rec("客户合同_签字版.pdf", 32, work, 30, false, 2_411_724, 6));
        ix.add_entry(rec("索引性能测试数据.csv", 33, work, 30, false, 3_145_728, 8));
        let music = ix.add_entry(rec("Music", 40, root, 5, true, 0, 9));
        ix.add_entry(rec("lo-fi_beats.mp3", 41, music, 40, false, 7_340_032, 3));
        ix.add_entry(rec("会议录音_1026.m4a", 42, music, 40, false, 6_410_000, 1));
        let mut cfg = Config::default();
        cfg.ui.theme = theme;
        let pj = |parts: &[&str]| {
            let mut out = base_s.clone();
            for p in parts {
                out.push('\\');
                out.push_str(p);
            }
            out
        };
        cfg.marks.insert(pj(&["Photos", "sunset_2026.png"]), 3);
        cfg.marks.insert(pj(&["readme.txt"]), 0);
        cfg.marks.insert(pj(&["工作", "产品需求文档_v3.2.docx"]), 5);



        let status = StatusHandle::new(
            ctx.clone(),
            Status {
                phase: "就绪 · MFT 索引 · 13 项".into(),
                ready: true,
                volumes_done: 1,
                volumes_total: 1,
                error: None,
                needs_rescan: false,
                mft_mode: true,
                started: std::time::Instant::now(),
            },
        );
        let (excl_tx, _) = std::sync::mpsc::channel();
        let (refresh_tx, _) = std::sync::mpsc::channel();
        let handle = IndexHandle { index: Arc::new(RwLock::new(ix)), status, excl_tx, refresh_tx };
        let app = App::with_parts(ctx.clone(), cfg, handle);
        (ctx, app)
    }

    fn harness(app: App, w: f32, h: f32) -> Harness<'static, App> {
        Harness::builder()
            .with_size(egui::vec2(w, h))
            .build_state(|ctx, app: &mut App| app.update_frame(ctx), app)
    }

    /// 等预览线程把像素送回来再截图，否则快照会拍到「正在读取…」。
    fn settle_preview(h: &mut Harness<'static, App>) {
        for _ in 0..60 {
            h.run();
            let busy = h.state().preview.as_ref().map_or(false, |p| p.loading);
            if !busy {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        settle(h);
    }

    fn settle(h: &mut Harness<'static, App>) {
        for _ in 0..3 {
            h.run();
        }
        // 关闭文本光标闪烁，否则截图会因光标相位不同而抖动
        let mut st = (*h.ctx.style()).clone();
        st.visuals.text_cursor.blink = false;
        h.ctx.set_style(st);
        h.run();
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p1_default() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        h.wgpu_snapshot("probe_1_default");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p2_selected() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        let mut nodes: Vec<_> = h.query_all_by_label_contains("sunset_2026.png").collect();
        nodes.swap_remove(0).click();
        settle_preview(&mut h);
        h.wgpu_snapshot("probe_2_selected");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p3_settings() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        h.get_by_label("设置").click();
        settle(&mut h);
        h.wgpu_snapshot("probe_3_settings");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p4_mid() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 900.0, 640.0);
        settle(&mut h);
        h.wgpu_snapshot("probe_4_mid");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p5_narrow() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 660.0, 640.0);
        settle(&mut h);
        h.wgpu_snapshot("probe_5_narrow");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p7_hover() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        for _ in 0..2 {
            h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(420.0, 272.0)));
            h.run();
        }
        h.wgpu_snapshot("probe_7_hover");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p8_settings_narrow() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 700.0, 520.0);
        settle(&mut h);
        h.get_by_label("设置").click();
        settle(&mut h);
        h.wgpu_snapshot("probe_8_settings_narrow");
    }

    #[test]
    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p9_image_preview() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        let mut nodes: Vec<_> = h.query_all_by_label_contains("sunset_2026.png").collect();
        nodes.swap_remove(0).click();
        settle_preview(&mut h);
        h.wgpu_snapshot("probe_9_image");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p10_text_preview() {
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        let mut nodes: Vec<_> = h.query_all_by_label_contains("readme.txt").collect();
        nodes.swap_remove(0).click();
        settle_preview(&mut h);
        h.wgpu_snapshot("probe_10_text");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p11_video_missing() {
        // 条目存在但文件不在磁盘：应优雅降级，不 panic
        let (_, app) = build(ThemePref::Light);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        let mut nodes: Vec<_> = h.query_all_by_label_contains("holiday.mp4").collect();
        nodes.swap_remove(0).click();
        settle_preview(&mut h);
        h.wgpu_snapshot("probe_11_video_missing");
    }

    #[test]
    #[ignore = "人工核对界面用：UPDATE_SNAPSHOTS=1 cargo test ui_probe -- --ignored"]
    fn p6_dark() {
        let (_, app) = build(ThemePref::Dark);
        let mut h = harness(app, 1280.0, 820.0);
        settle(&mut h);
        let mut nodes: Vec<_> = h.query_all_by_label_contains("sunset_2026.png").collect();
        nodes.swap_remove(0).click();
        settle_preview(&mut h);
        h.wgpu_snapshot("probe_6_dark");
    }
}

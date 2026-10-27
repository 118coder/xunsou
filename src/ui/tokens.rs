//! 设计令牌：颜色、字号、间距、圆角、密度。
//!
//! 单一强调色（brand）+ 中性灰阶骨架；分类色与标记色只做小面积着色。
//! 深/浅两套等价取值，视图层只认语义名，不直接写死 hex。

use egui::{Color32, Context, FontId, Stroke, TextStyle};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePref {
    pub const ALL: [ThemePref; 3] = [ThemePref::System, ThemePref::Light, ThemePref::Dark];
    pub fn label(self) -> &'static str {
        match self {
            ThemePref::System => "跟随系统",
            ThemePref::Light => "浅色",
            ThemePref::Dark => "深色",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Density {
    Compact,
    #[default]
    Normal,
    Comfortable,
}

impl Density {
    pub const ALL: [Density; 3] = [Density::Compact, Density::Normal, Density::Comfortable];
    pub fn row(self) -> f32 {
        match self {
            Density::Compact => 28.0,
            Density::Normal => 32.0,
            Density::Comfortable => 40.0,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Density::Compact => "紧凑",
            Density::Normal => "标准",
            Density::Comfortable => "宽松",
        }
    }
}

/// 语义色板。Copy 语义，每帧按需取一份即可。
#[derive(Clone, Copy)]
pub struct Pal {
    pub dark: bool,
    pub bg: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub raised: Color32,
    pub border: Color32,
    pub border2: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    pub brand: Color32,
    pub brand_soft: Color32,
    pub brand_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub on_brand: Color32,
}

pub fn pal(pref: ThemePref, system_dark: bool) -> Pal {
    let dark = match pref {
        ThemePref::Dark => true,
        ThemePref::Light => false,
        ThemePref::System => system_dark,
    };
    if dark {
        Pal {
            dark,
            bg: Color32::from_rgb(0x0F, 0x11, 0x15),
            surface: Color32::from_rgb(0x16, 0x19, 0x1F),
            hover: Color32::from_rgb(0x1B, 0x1F, 0x26),
            raised: Color32::from_rgb(0x22, 0x27, 0x2F),
            border: Color32::from_rgb(0x26, 0x2B, 0x33),
            border2: Color32::from_rgb(0x34, 0x3B, 0x45),
            text: Color32::from_rgb(0xE7, 0xEA, 0xF0),
            text2: Color32::from_rgb(0x9D, 0xA6, 0xB4),
            text3: Color32::from_rgb(0x7A, 0x84, 0x94),
            brand: Color32::from_rgb(0x3B, 0x9E, 0xFF),
            // 实色预混：egui 在线性空间做 alpha 混合，半透明品牌色会明显偏亮，
            // 这里直接给「bg 上 12% 品牌色」的 sRGB 结果，与 HTML 原型一致
            brand_soft: Color32::from_rgb(0x16, 0x28, 0x3A),
            brand_text: Color32::from_rgb(0x7F, 0xC0, 0xFF),
            ok: Color32::from_rgb(0x3D, 0xD6, 0x8C),
            warn: Color32::from_rgb(0xF5, 0xA5, 0x24),
            danger: Color32::from_rgb(0xFF, 0x6B, 0x6B),
            on_brand: Color32::WHITE,
        }
    } else {
        Pal {
            dark,
            bg: Color32::WHITE,
            surface: Color32::from_rgb(0xF7, 0xF8, 0xFA),
            hover: Color32::from_rgb(0xF1, 0xF3, 0xF6),
            raised: Color32::WHITE,
            border: Color32::from_rgb(0xE3, 0xE6, 0xEB),
            border2: Color32::from_rgb(0xCB, 0xD2, 0xDA),
            text: Color32::from_rgb(0x11, 0x18, 0x27),
            text2: Color32::from_rgb(0x4B, 0x55, 0x63),
            text3: Color32::from_rgb(0x6B, 0x72, 0x80),
            brand: Color32::from_rgb(0x0B, 0x6B, 0xCB),
            brand_soft: Color32::from_rgb(0xEA, 0xF2, 0xFC),
            brand_text: Color32::from_rgb(0x0B, 0x6B, 0xCB),
            ok: Color32::from_rgb(0x17, 0x80, 0x3D),
            warn: Color32::from_rgb(0xB4, 0x53, 0x09),
            danger: Color32::from_rgb(0xC8, 0x1E, 0x1E),
            on_brand: Color32::WHITE,
        }
    }
}

/// 分类色：只用于 16px 图标着色，不参与大面积填充。
///
/// 统一 S≈55% / L≈48% 的低饱和带——高饱和多彩图标会让侧栏和表格整体"发花"，
/// 注意力从内容跑到色块上。识别性靠色相区分，不靠纯度。
pub const CAT_COLORS: [Color32; 7] = [
    Color32::from_rgb(0x0B, 0x6B, 0xCB), // 全部 = brand
    Color32::from_rgb(0x2E, 0x9E, 0x7B), // 图片
    Color32::from_rgb(0x3B, 0x7D, 0xD8), // 文档
    Color32::from_rgb(0xD6, 0x5A, 0x54), // 视频
    Color32::from_rgb(0x8B, 0x6B, 0xD1), // 音频
    Color32::from_rgb(0xC9, 0x90, 0x2B), // 手机
    Color32::from_rgb(0x6E, 0x7C, 0x8F), // 目录
];

/// 8 色标记：色相均匀、亮度一致。
pub const MARKS: [Color32; 8] = [
    Color32::from_rgb(0xEF, 0x44, 0x44),
    Color32::from_rgb(0xF9, 0x73, 0x16),
    Color32::from_rgb(0xF5, 0x9E, 0x0B),
    Color32::from_rgb(0x22, 0xC5, 0x5E),
    Color32::from_rgb(0x14, 0xB8, 0xA6),
    Color32::from_rgb(0x3B, 0x82, 0xF6),
    Color32::from_rgb(0x8B, 0x5C, 0xF6),
    Color32::from_rgb(0x64, 0x74, 0x8B),
];

pub mod size {
    pub const TOPBAR: f32 = 64.0;
    pub const STATUS: f32 = 26.0;
    pub const SIDEBAR: f32 = 208.0;
    pub const SIDEBAR_MINI: f32 = 56.0;
    pub const PREVIEW: f32 = 300.0;
    pub const PREVIEW_MID: f32 = 268.0;
    pub const HEADER: f32 = 32.0;
    pub const RADIUS: f32 = 6.0;
    pub const SEARCH_H: f32 = 38.0;
}

/// 把令牌写进 egui 全局样式。主题/密度变化时重新调用。
pub fn apply(ctx: &Context, p: &Pal, d: Density) {
    let mut style = (*ctx.style()).clone();
    style.visuals = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    {
        let v = &mut style.visuals;
        v.panel_fill = p.bg;
        v.window_fill = if p.dark { p.raised } else { p.surface };
        // 气泡/弹出层：浅色主题下必须与白底表格区分开，否则看不出边界
        v.extreme_bg_color = if p.dark { p.raised } else { p.surface };
        v.faint_bg_color = p.surface;
        v.window_stroke = Stroke::new(1.0, p.border2);
        v.window_rounding = egui::Rounding::same(8.0);
        v.window_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 8.0),
            blur: 24.0,
            spread: 0.0,
            color: Color32::from_black_alpha(if p.dark { 90 } else { 26 }),
        };
        v.popup_shadow = v.window_shadow;
        v.widgets.noninteractive.bg_fill = p.surface;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.inactive.bg_fill = p.surface;
        v.widgets.inactive.weak_bg_fill = p.surface;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text2);
        // 勾选框的方框取自 bg_stroke，缺省近乎透明，浅色下几乎看不见
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border2);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.text3);
        v.widgets.active.bg_stroke = Stroke::new(1.0, p.brand);
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.active.bg_fill = p.brand_soft;
        v.widgets.active.weak_bg_fill = p.brand_soft;
        v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
        v.selection.bg_fill = p.brand_soft;
        v.selection.stroke = Stroke::new(1.0, p.brand);
        v.hyperlink_color = p.brand;
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.rounding = egui::Rounding::same(size::RADIUS);
        }
    }
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size = egui::vec2(24.0, 24.0);
    style.spacing.scroll.bar_width = 8.0;
    style.text_styles.insert(TextStyle::Body, FontId::proportional(13.0));
    style.text_styles.insert(TextStyle::Button, FontId::proportional(13.0));
    style.text_styles.insert(TextStyle::Small, FontId::proportional(11.0));
    style.text_styles.insert(TextStyle::Monospace, FontId::monospace(12.0));
    style.text_styles.insert(TextStyle::Heading, FontId::proportional(15.0));
    let _ = d;
    ctx.set_style(style);
}

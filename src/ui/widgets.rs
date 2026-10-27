//! 通用控件：图标按钮、chip、kbd、分区标题、色点。
//!
//! 全部自绘，但都通过 `widget_info` 进入无障碍树，保证屏幕阅读器与
//! 无头测试都能按 label 定位。

use egui::{Color32, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType};

use super::icons::{self, Icon};
use super::tokens::{self, Pal};

/// 28×28 的图标按钮。`label` 同时作为 tooltip 与无障碍标签。
pub fn icon_button(ui: &mut Ui, p: &Pal, icon: Icon, label: &str, active: bool, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    let bg = if active {
        p.brand_soft
    } else if resp.hovered() {
        p.hover
    } else {
        Color32::TRANSPARENT
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, tokens::size::RADIUS, bg);
    }
    let fg = if active {
        p.brand
    } else if resp.hovered() {
        p.text
    } else {
        p.text2
    };
    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(16.0)), icon, fg, 1.5);
    resp.on_hover_text(label)
}

/// 文字按钮：主/次两种。
pub fn button(ui: &mut Ui, p: &Pal, text: &str, primary: bool, min_w: f32) -> Response {
    let h = 32.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(min_w.max(56.0), h), Sense::click());
    let (bg, fg, stroke) = if !ui.is_enabled() {
        (p.surface, p.text3, Stroke::new(1.0, p.border))
    } else if primary {
        let bg = if resp.hovered() { p.brand.gamma_multiply(1.1) } else { p.brand };
        (bg, p.on_brand, Stroke::NONE)
    } else if resp.hovered() {
        (p.hover, p.text, Stroke::new(1.0, p.border2))
    } else {
        (p.surface, p.text, Stroke::new(1.0, p.border))
    };
    ui.painter().rect_filled(rect, tokens::size::RADIUS, bg);
    if stroke != Stroke::NONE {
        ui.painter().rect_stroke(rect, tokens::size::RADIUS, stroke);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(13.0),
        fg,
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), text));
    resp
}

/// 胶囊 chip（窄屏分类条）。
pub fn chip(ui: &mut Ui, p: &Pal, text: &str, active: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), egui::FontId::proportional(12.5), p.text);
    let w = galley.size().x + 20.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
    let (bg, fg, stroke) = if active {
        (p.brand_soft, p.brand_text, Stroke::new(1.0, p.brand))
    } else if resp.hovered() {
        (p.hover, p.text, Stroke::new(1.0, p.border2))
    } else {
        (Color32::TRANSPARENT, p.text2, Stroke::new(1.0, p.border2))
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 999.0, bg);
    }
    ui.painter().rect_stroke(rect, 999.0, stroke);
    ui.painter().galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, text));
    resp
}

/// 键帽提示（非交互）。
#[allow(dead_code)]
pub fn kbd(ui: &mut Ui, p: &Pal, text: &str) {
    let font = egui::FontId::proportional(10.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, p.text3);
    let w = galley.size().x + 10.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 17.0), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, p.bg);
    ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, p.border2));
    ui.painter().galley(
        egui::pos2(rect.center().x - galley.size().x / 2.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text3,
    );
}

/// 分区标题（11px）。用真实 Label 渲染，保证文字进入无障碍树
/// （屏幕阅读器与无头测试都按 label 定位）。
pub fn section(ui: &mut Ui, p: &Pal, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(p.text3)));
}

/// 1px 分隔线。
pub fn divider(ui: &mut Ui, p: &Pal) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
    let y = rect.center().y;
    ui.painter().line_segment(
        [egui::pos2(rect.left() + 4.0, y), egui::pos2(rect.right() - 4.0, y)],
        Stroke::new(1.0, p.border),
    );
}

/// 色点。
pub fn dot(ui: &mut Ui, color: Color32, d: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
    ui.painter().circle_filled(rect.center(), d / 2.0, color);
}

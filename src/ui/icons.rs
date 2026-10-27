//! 线性图标：用 Painter 直接绘制，零依赖、随主题着色、不依赖 emoji 字体。
//!
//! 所有图标在 16×16 的逻辑网格内定义，按目标尺寸等比缩放，线宽固定 1.5。
//! 相比引入字体图标库：体积为零、可单测、色值跟随语义令牌。

use egui::{pos2, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Bolt,
    Search,
    Close,
    Chevron,
    Folder,
    Image,
    Doc,
    Video,
    Audio,
    Phone,
    Tag,
    Trash,
    Settings,
    Refresh,
    Copy,
    External,
    Check,
    Grid,
    Panel,
    Disk,
    Shield,
    Sun,
    Moon,
    More,
}

/// 在 `rect` 指定的方框内绘制图标（调用方负责保证 rect 是正方形）。
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32, width: f32) {
    let s = rect.width() / 16.0;
    let st = Stroke::new(width * s.max(0.6), color);
    let x = |v: f32| rect.min.x + v * s;
    let y = |v: f32| rect.min.y + v * s;
    let pt = |a: f32, b: f32| pos2(x(a), y(b));
    let line = |a: f32, b: f32, c: f32, d: f32| painter.line_segment([pt(a, b), pt(c, d)], st);

    match icon {
        Icon::Bolt => {
            painter.add(Shape::closed_line(
                vec![pt(9.2, 1.4), pt(3.4, 9.1), pt(6.8, 9.1), pt(6.2, 14.6), pt(12.6, 6.4), pt(9.1, 6.4)],
                st,
            ));
        }
        Icon::Search => {
            painter.circle_stroke(pt(7.0, 7.0), 4.4 * s, st);
            line(10.4, 10.4, 14.0, 14.0);
        }
        Icon::Close => {
            line(4.2, 4.2, 11.8, 11.8);
            line(11.8, 4.2, 4.2, 11.8);
        }
        Icon::Chevron => {
            line(4.6, 6.4, 8.0, 9.8);
            line(8.0, 9.8, 11.4, 6.4);
        }
        Icon::Folder => {
            painter.add(Shape::closed_line(
                vec![
                    pt(2.4, 12.6),
                    pt(2.4, 5.0),
                    pt(6.4, 5.0),
                    pt(7.9, 6.9),
                    pt(13.6, 6.9),
                    pt(13.6, 12.6),
                ],
                st,
            ));
        }
        Icon::Image => {
            painter.rect_stroke(
                Rect::from_min_max(pt(2.0, 3.0), pt(14.0, 13.0)),
                1.6 * s,
                st,
            );
            painter.circle_stroke(pt(5.9, 6.3), 1.1 * s, st);
            painter.add(Shape::line(
                vec![pt(2.6, 11.8), pt(6.0, 8.4), pt(8.4, 10.8), pt(10.3, 8.9), pt(13.4, 12.0)],
                st,
            ));
        }
        Icon::Doc => {
            painter.add(Shape::closed_line(
                vec![
                    pt(9.2, 1.8),
                    pt(4.8, 1.8),
                    pt(3.3, 3.3),
                    pt(3.3, 12.7),
                    pt(4.8, 14.2),
                    pt(11.2, 14.2),
                    pt(12.7, 12.7),
                    pt(12.7, 5.3),
                ],
                st,
            ));
            line(9.2, 1.8, 9.2, 5.3);
            line(9.2, 5.3, 12.7, 5.3);
        }
        Icon::Video => {
            painter.rect_stroke(Rect::from_min_max(pt(1.8, 4.0), pt(11.0, 12.0)), 1.6 * s, st);
            painter.add(Shape::closed_line(
                vec![pt(11.0, 7.6), pt(14.2, 6.0), pt(14.2, 10.0), pt(11.0, 8.4)],
                st,
            ));
        }
        Icon::Audio => {
            line(6.2, 11.6, 6.2, 4.4);
            line(6.2, 4.4, 12.2, 3.2);
            line(12.2, 3.2, 12.2, 10.4);
            painter.circle_stroke(pt(4.6, 11.8), 1.6 * s, st);
            painter.circle_stroke(pt(10.6, 10.6), 1.6 * s, st);
        }
        Icon::Phone => {
            painter.rect_stroke(Rect::from_min_max(pt(4.4, 1.8), pt(11.6, 14.2)), 1.6 * s, st);
            line(6.9, 11.9, 9.1, 11.9);
        }
        Icon::Tag => {
            painter.add(Shape::closed_line(
                vec![pt(2.4, 2.4), pt(7.4, 2.4), pt(13.6, 8.6), pt(8.6, 13.6), pt(2.4, 7.4)],
                st,
            ));
            painter.circle_stroke(pt(5.6, 5.6), 0.9 * s, st);
        }
        Icon::Settings => {
            line(2.4, 5.0, 13.6, 5.0);
            line(2.4, 11.0, 13.6, 11.0);
            painter.circle_stroke(pt(6.0, 5.0), 1.7 * s, st);
            painter.circle_stroke(pt(10.4, 11.0), 1.7 * s, st);
        }
        Icon::Refresh => {
            let mut pts = Vec::with_capacity(22);
            let c = pt(8.0, 8.0);
            let r = 5.0 * s;
            for k in 0..=18 {
                let a = -0.35 + (k as f32 / 18.0) * 5.1;
                pts.push(pos2(c.x + r * a.cos(), c.y + r * a.sin()));
            }
            painter.add(Shape::line(pts, st));
            // 箭头
            let tip = pt(8.0, 3.0);
            painter.add(Shape::line(vec![pos2(tip.x - 2.6 * s, tip.y - 0.2 * s), tip, pos2(tip.x + 0.2 * s, tip.y + 2.6 * s)], st));
        }
        Icon::Copy => {
            painter.rect_stroke(Rect::from_min_max(pt(5.4, 5.4), pt(13.4, 13.4)), 1.2 * s, st);
            painter.add(Shape::line(
                vec![pt(10.4, 5.4), pt(10.4, 3.8), pt(9.2, 2.6), pt(3.8, 2.6), pt(2.6, 3.8), pt(2.6, 9.2), pt(3.8, 10.4), pt(5.4, 10.4)],
                st,
            ));
        }
        Icon::External => {
            line(9.4, 2.6, 13.4, 2.6);
            line(13.4, 2.6, 13.4, 6.6);
            line(13.4, 2.6, 7.6, 8.4);
            painter.add(Shape::line(
                vec![pt(11.2, 9.4), pt(11.2, 11.8), pt(9.8, 13.2), pt(3.6, 13.2), pt(2.2, 11.8), pt(2.2, 5.6), pt(3.6, 4.2), pt(6.0, 4.2)],
                st,
            ));
        }
        Icon::Check => {
            painter.add(Shape::line(vec![pt(3.4, 8.4), pt(6.4, 11.4), pt(12.6, 4.8)], st));
        }
        Icon::Grid => {
            line(2.6, 4.6, 13.4, 4.6);
            line(2.6, 8.0, 13.4, 8.0);
            line(2.6, 11.4, 13.4, 11.4);
        }
        Icon::Panel => {
            painter.rect_stroke(Rect::from_min_max(pt(2.0, 3.0), pt(14.0, 13.0)), 1.5 * s, st);
            line(10.0, 3.0, 10.0, 13.0);
        }
        Icon::Disk => {
            painter.rect_stroke(Rect::from_min_max(pt(2.2, 3.0), pt(13.8, 13.0)), 2.0 * s, st);
            line(2.2, 9.4, 13.8, 9.4);
            painter.circle_filled(pt(11.6, 11.2), 0.75 * s, color);
        }
        Icon::Shield => {
            painter.add(Shape::closed_line(
                vec![pt(8.0, 1.8), pt(13.0, 3.6), pt(13.0, 7.7), pt(11.0, 11.7), pt(8.0, 13.9), pt(5.0, 11.7), pt(3.0, 7.7), pt(3.0, 3.6)],
                st,
            ));
        }
        Icon::Sun => {
            painter.circle_stroke(pt(8.0, 8.0), 2.4 * s, st);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::PI / 4.0;
                let (sx, sy) = (a.cos(), a.sin());
                line(8.0 + sx * 4.6, 8.0 + sy * 4.6, 8.0 + sx * 5.9, 8.0 + sy * 5.9);
            }
        }
        Icon::Moon => {
            // 月牙 = 大圆挖去偏移小圆。两段弧必须共用交点，否则
            // Shape::closed_line 会补一条弦，在 16px 下表现为一道斜穿线。
            let (cx, cy, r) = (7.7f32, 8.2f32, 5.6f32);
            let (ix, iy, ir) = (11.4f32, 6.2f32, 5.0f32);
            let (dx, dy) = (ix - cx, iy - cy);
            let d = (dx * dx + dy * dy).sqrt();
            if d > 0.01 && d < r + ir && (r - ir).abs() < d {
                let a = (r * r - ir * ir + d * d) / (2.0 * d);
                let h = (r * r - a * a).max(0.0).sqrt();
                let (ux, uy) = (dx / d, dy / d);
                let (mx, my) = (cx + a * ux, cy + a * uy);
                let (p1x, p1y) = (mx - h * uy, my + h * ux);
                let (p2x, p2y) = (mx + h * uy, my - h * ux);

                let a1 = (p1y - cy).atan2(p1x - cx);
                let a2 = {
                    let v = (p2y - cy).atan2(p2x - cx);
                    if v < a1 {
                        v + std::f32::consts::TAU
                    } else {
                        v
                    }
                };
                let b1 = (p1y - iy).atan2(p1x - ix);
                let b2 = {
                    let v = (p2y - iy).atan2(p2x - ix);
                    if v < b1 {
                        v + std::f32::consts::TAU
                    } else {
                        v
                    }
                };

                let mut pts: Vec<egui::Pos2> = Vec::with_capacity(40);
                // 外弧：沿大圆逆时针，走外侧长弧
                for k in 0..=20 {
                    let ang = a1 + (a2 - a1) * (k as f32 / 20.0);
                    pts.push(pos2(x(cx + r * ang.cos()), y(cy + r * ang.sin())));
                }
                // 内弧：从交点回到起点，沿小圆顺时针，走内侧短弧
                for k in 0..=16 {
                    let ang = b2 + (b1 - b2) * (k as f32 / 16.0);
                    pts.push(pos2(x(ix + ir * ang.cos()), y(iy + ir * ang.sin())));
                }
                painter.add(Shape::closed_line(pts, st));
            }
        }
        Icon::Trash => {
            // 垃圾桶：盖 + 桶身两道竖纹
            line(4.6, 4.4, 11.4, 4.4);
            line(8.0, 3.0, 8.0, 4.4);
            painter.add(Shape::line(
                vec![pt(5.2, 4.4), pt(5.9, 13.4), pt(10.1, 13.4), pt(10.8, 4.4)],
                st,
            ));
            line(6.9, 7.0, 7.0, 11.0);
            line(9.1, 7.0, 9.0, 11.0);
        }
        Icon::More => {
            for dx in [3.6f32, 8.0, 12.4] {
                painter.circle_filled(pt(dx, 8.0), 0.95 * s, color);
            }
        }
    }
}

/// 非交互图标：占位 `size`，返回 Response 便于 tooltip。
pub fn image(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(size)), icon, color, 1.5);
    resp
}

/// 分类 → 图标。
pub fn for_category(cat: crate::categories::Category) -> Icon {
    use crate::categories::Category as C;
    match cat {
        C::All => Icon::Grid,
        C::Image => Icon::Image,
        C::Doc => Icon::Doc,
        C::Video => Icon::Video,
        C::Audio => Icon::Audio,
        C::Phone => Icon::Phone,
        C::Dir => Icon::Folder,
    }
}

/// 文件（按名称与是否目录）→ 图标。
pub fn for_file(name: &str, is_dir: bool) -> Icon {
    use crate::categories::{classify, Category as C};
    if is_dir {
        return Icon::Folder;
    }
    match classify(name, false) {
        Some(C::Image) => Icon::Image,
        Some(C::Doc) => Icon::Doc,
        Some(C::Video) => Icon::Video,
        Some(C::Audio) => Icon::Audio,
        Some(C::Phone) => Icon::Phone,
        _ => Icon::Doc,
    }
}

/// 分类 → 颜色下标（与 CAT_COLORS 对应）。
pub fn cat_index(cat: crate::categories::Category) -> usize {
    use crate::categories::Category as C;
    match cat {
        C::All => 0,
        C::Image => 1,
        C::Doc => 2,
        C::Video => 3,
        C::Audio => 4,
        C::Phone => 5,
        C::Dir => 6,
    }
}

/// 文件 → 分类颜色下标。
pub fn file_cat_index(name: &str, is_dir: bool) -> usize {
    use crate::categories::{classify, Category as C};
    if is_dir {
        return 6;
    }
    match classify(name, false) {
        Some(C::Image) => 1,
        Some(C::Doc) => 2,
        Some(C::Video) => 3,
        Some(C::Audio) => 4,
        Some(C::Phone) => 5,
        _ => 0,
    }
}

/// 供布局用的点（未使用但保留以支持外部扩展）。
#[allow(dead_code)]
pub fn grid_pos(base: Pos2, dx: f32, dy: f32) -> Pos2 {
    pos2(base.x + dx, base.y + dy)
}

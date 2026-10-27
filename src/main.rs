//! 迅搜 —— 全盘极速文件搜索（Rust 重制版）
//!
//! 索引：NTFS MFT 直读（管理员）或目录遍历（用户显式授权）；
//! 实时：USN 日志增量更新；搜索：rayon 并行匹配。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod categories;
mod config;
mod indexer;
mod model;
mod search;
mod thumb;
mod ui;
mod win;

use app::App;

/// 窗口 / 任务栏图标：与 exe 资源里的 app.ico 同源（32px PNG）。
fn load_window_icon() -> std::sync::Arc<egui::IconData> {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/app_icon.png").as_slice())
        .unwrap_or_default()
        .into()
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("迅搜 · 全盘极速文件搜索")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([720.0, 480.0])
            // 窗口 / 任务栏图标：与 exe 资源里的 app.ico 同源；加载失败退回默认图标
            .with_icon(load_window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "迅搜 · 全盘极速文件搜索",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

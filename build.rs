//! 构建脚本：把应用图标接入 exe。
//!
//! 链接器对 .res 的内置解析比较挑剔，所以交给它的是 **cvtres 预转换的
//! COFF 目标文件** `assets/app.obj`（来源 `assets/app.res`，由
//! `assets/make_res.py` 按 32 位 .RES 规范生成，再经 MSVC 的
//! cvtres.exe 转换）。换图标时的完整流程：
//!
//! 1. 更新 `assets/app.ico`（内含 16/24/32/48/64/128/256 各一档 PNG）
//! 2. `python assets/make_res.py` 重新生成 app.res
//! 3. `cvtres /MACHINE:X64 /OUT:assets/app.obj assets/app.res`
//! 4. 重新构建

fn main() {
    println!("cargo:rerun-if-changed=assets/app.obj");
    #[cfg(windows)]
    println!("cargo:rustc-link-arg-bins=assets/app.obj");
}

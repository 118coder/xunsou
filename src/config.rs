//! 配置：设置项、标记文件、开机自启、注册表热键持久化。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ui::tokens::{Density, ThemePref};

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub autostart: bool,
    pub hotkey: String,
    pub excluded: Vec<String>,
    /// 标记文件：完整路径 → 颜色序号
    #[serde(default)]
    pub marks: std::collections::BTreeMap<String, u8>,
    /// 界面偏好：主题、密度、侧栏折叠等。旧配置缺字段时走默认值。
    #[serde(default)]
    pub ui: UiPrefs,
    /// 是否允许在 MFT 不可用时回退到全盘目录遍历（默认关闭，需用户显式授权）
    #[serde(default)]
    pub allow_walk: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            autostart: false,
            hotkey: "Alt + `".into(),
            excluded: Vec::new(),
            marks: Default::default(),
            ui: UiPrefs::default(),
            allow_walk: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct UiPrefs {
    pub theme: ThemePref,
    pub density: Density,
    pub sidebar_collapsed: bool,
    pub preview_open: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        UiPrefs {
            theme: ThemePref::System,
            density: Density::Normal,
            sidebar_collapsed: false,
            preview_open: true,
        }
    }
}

pub fn app_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("xunsou")
}

pub fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

pub fn index_path() -> PathBuf {
    app_dir().join("index.bin")
}

pub fn load() -> Config {
    match std::fs::read(config_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) {
    let path = config_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(cfg) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "XunSou";

pub fn set_autostart(enable: bool) -> Result<(), String> {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    let key = hkcu.open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE).map_err(|e| e.to_string())?;
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let cmd = format!("\"{}\"", exe.display());
        key.set_value(RUN_NAME, &cmd).map_err(|e| e.to_string())
    } else {
        key.delete_value(RUN_NAME).or(Ok(())).map_err(|e: std::io::Error| e.to_string())
    }
}

pub fn autostart_enabled() -> bool {
    let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    hkcu.open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_NAME))
        .is_ok()
}

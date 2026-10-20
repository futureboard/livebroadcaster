//! Where the scene collection and settings live: `%APPDATA%\LiveBroadcaster\` (JSON), or
//! `LB_CONFIG_DIR` when set.

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::model::{SceneCollection, Settings};

pub fn config_dir() -> PathBuf {
    // A separate profile (testing, portable setups).
    if let Some(d) = std::env::var_os("LB_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("", "", "LiveBroadcaster")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load<T: DeserializeOwned>(name: &str) -> Option<T> {
    let path = config_dir().join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("{}: {e} (keeping a backup, starting fresh)", path.display());
            let _ = std::fs::copy(&path, path.with_extension("json.bak"));
            None
        }
    }
}

fn save<T: Serialize>(name: &str, v: &T) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

pub fn load_collection() -> SceneCollection {
    let mut c = load::<SceneCollection>("scenes.json").unwrap_or_else(SceneCollection::new_default);
    c.repair();
    c
}

pub fn save_collection(c: &SceneCollection) -> Result<(), String> {
    save("scenes.json", c)
}

pub fn load_settings() -> Settings {
    load("settings.json").unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> Result<(), String> {
    save("settings.json", s)
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt, path::PathBuf};

pub fn directory() -> Result<PathBuf> {
    let directory = if let Some(path) = std::env::var_os("OTO_CONFIG_DIR") {
        PathBuf::from(path)
    } else {
        PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?)
            .join("Library/Application Support/Oto")
    };
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    pub device: Option<String>,
    #[serde(default)]
    pub offsets: BTreeMap<String, i32>,
}
impl Settings {
    pub fn load() -> Result<Self> {
        let path = directory()?.join("settings.json");
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid Oto settings.json"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        let directory = directory()?;
        let temp = directory.join(format!("settings-{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temp, directory.join("settings.json"))?;
        Ok(())
    }
    pub fn latency(&self) -> i32 {
        *self
            .offsets
            .get(self.device.as_deref().unwrap_or("default"))
            .unwrap_or(&0)
    }
    pub fn set_latency(&mut self, ms: i32) {
        self.offsets
            .insert(self.device.clone().unwrap_or_else(|| "default".into()), ms);
    }
}

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
    /// Actual output reported by the engine; not the user's output preference.
    #[serde(skip)]
    pub active_output: Option<String>,
    /// Legacy additional local offsets. Preserve them without interpreting them
    /// as estimates of a speaker's physical delay.
    #[serde(default)]
    pub offsets: BTreeMap<String, i32>,
    #[serde(default)]
    pub speaker_delays: BTreeMap<String, i32>,
}
impl Settings {
    pub fn load() -> Result<Self> {
        let path = directory()?.join("settings.json");
        match fs::read(path) {
            Ok(bytes) => {
                let settings: Self =
                    serde_json::from_slice(&bytes).context("Invalid Oto settings.json")?;
                anyhow::ensure!(
                    settings
                        .speaker_delays
                        .values()
                        .all(|ms| (0..=500).contains(ms)),
                    "Saved speaker delay must be between 0 and 500 ms"
                );
                Ok(settings)
            }
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
        *self.speaker_delays.get(self.latency_key()).unwrap_or(&0)
    }
    pub fn set_latency(&mut self, ms: i32) {
        self.speaker_delays
            .insert(self.latency_key().to_owned(), ms);
    }
    fn latency_key(&self) -> &str {
        self.active_output
            .as_deref()
            .or(self.device.as_deref())
            .unwrap_or("default")
    }
    pub fn activate_output(&mut self, uid: Option<String>) {
        self.active_output = uid;
    }
}

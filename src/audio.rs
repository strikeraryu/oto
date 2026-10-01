use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::Stdio,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::mpsc,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Device {
    pub id: u32,
    pub uid: String,
    pub name: String,
    pub sample_rate: f64,
    pub is_default: bool,
    pub latency_frames: u32,
}

pub fn helper_path() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        static BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/oto-audio"));
        let hash = format!("{:x}", Sha256::digest(BINARY));
        let directory = crate::settings::directory()?
            .join("engines")
            .join(&hash[..16]);
        fs::create_dir_all(&directory)?;
        let path = directory.join("oto-audio");
        let existing = fs::read(&path).ok();
        if existing.as_deref() != Some(BINARY) {
            let temp = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&temp)?;
            std::io::Write::write_all(&mut file, BINARY)?;
            file.sync_all()?;
            fs::rename(temp, &path)?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("Oto audio currently supports macOS 14.2 or later")
    }
}

pub async fn devices() -> Result<Vec<Device>> {
    let output = Command::new(helper_path()?).arg("devices").output().await?;
    ensure!(
        output.status.success(),
        "Audio device enumeration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("Invalid audio device response")
}

pub struct Capture {
    pub timestamp: u64,
    pub pcm: Vec<u8>,
}
pub enum CommandFrame {
    Audio { timestamp: u64, pcm: Vec<u8> },
    Device(String),
}

pub struct AudioEngine {
    child: Option<Child>,
    pub sender: mpsc::Sender<CommandFrame>,
    pub captures: mpsc::Receiver<Capture>,
    pub errors: mpsc::Receiver<String>,
    pub device_changes: Option<mpsc::Receiver<Device>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    pub device: String,
}

async fn read_record(reader: &mut ChildStdout) -> Result<(u32, u64, Vec<u8>)> {
    let mut header = [0u8; 16];
    reader
        .read_exact(&mut header)
        .await
        .context("Audio engine exited; check capture permission and output device")?;
    let kind = u32::from_le_bytes(header[..4].try_into()?);
    let length = u32::from_le_bytes(header[4..8].try_into()?) as usize;
    let timestamp = u64::from_le_bytes(header[8..].try_into()?);
    ensure!(length <= 4096, "Audio engine record too large");
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok((kind, timestamp, bytes))
}
async fn write_frame(writer: &mut ChildStdin, frame: CommandFrame) -> Result<()> {
    let (kind, timestamp, bytes) = match frame {
        CommandFrame::Audio { timestamp, pcm } => (1u32, timestamp, pcm),
        CommandFrame::Device(uid) => (2u32, 0, uid.into_bytes()),
    };
    let mut header = Vec::with_capacity(16 + bytes.len());
    header.extend_from_slice(&kind.to_le_bytes());
    header.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    header.extend_from_slice(&timestamp.to_le_bytes());
    header.extend_from_slice(&bytes);
    writer.write_all(&header).await?;
    Ok(())
}

impl AudioEngine {
    pub async fn start(capture: bool, device: Option<&str>, headless: bool) -> Result<Self> {
        let (sender, mut input) = mpsc::channel(128);
        let (capture_tx, captures) = mpsc::channel(32);
        let (error_tx, errors) = mpsc::channel(4);
        let (device_tx, device_changes) = mpsc::channel(8);
        if headless {
            let task = tokio::spawn(async move {
                let _keep_channels_open = (capture_tx, error_tx, device_tx);
                while input.recv().await.is_some() {}
            });
            return Ok(Self {
                child: None,
                sender,
                captures,
                errors,
                device_changes: Some(device_changes),
                tasks: vec![task],
                device: "headless".into(),
            });
        }
        let mut child = Command::new(helper_path()?)
            .arg(if capture { "capture" } else { "play" })
            .arg(device.unwrap_or(""))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        let (kind, _, bytes) =
            tokio::time::timeout(std::time::Duration::from_secs(15), read_record(&mut stdout))
                .await
                .context("Audio engine startup timed out")??;
        ensure!(kind == 11, "Audio engine did not report ready");
        let device: Device = serde_json::from_slice(&bytes)?;
        let mut stdin = child.stdin.take().unwrap();
        let writer_errors = error_tx.clone();
        let writer = tokio::spawn(async move {
            while let Some(frame) = input.recv().await {
                if let Err(error) = write_frame(&mut stdin, frame).await {
                    let _ = writer_errors.send(error.to_string()).await;
                    break;
                }
            }
        });
        let reader = tokio::spawn(async move {
            loop {
                match read_record(&mut stdout).await {
                    Ok((10, timestamp, pcm)) if pcm.len() == crate::protocol::PAYLOAD => {
                        let _ = capture_tx.try_send(Capture { timestamp, pcm });
                    }
                    Ok((11, _, bytes)) => {
                        if let Ok(device) = serde_json::from_slice::<Device>(&bytes) {
                            let _ = device_tx.try_send(device);
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = error_tx.send(error.to_string()).await;
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child: Some(child),
            sender,
            captures,
            errors,
            device_changes: Some(device_changes),
            tasks: vec![writer, reader],
            device: device.name,
        })
    }
    pub async fn stop(&mut self) {
        // Cancel stdin writer first. EOF lets the helper restore original audio.
        for task in &self.tasks {
            task.abort();
        }
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        if let Some(child) = &mut self.child {
            if tokio::time::timeout(std::time::Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }
}
impl Drop for AudioEngine {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

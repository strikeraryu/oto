use anyhow::{bail, ensure, Result};
use clap::{Parser, Subcommand, ValueEnum};
use oto::{
    audio,
    session::{self, HostOptions, JoinOptions, LocalCommand},
    settings::Settings,
};
use std::net::{IpAddr, SocketAddr};

#[derive(Parser)]
#[command(
    name = "oto",
    version,
    about = "Synchronized audio for Macs on your local network"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum Source {
    System,
    Tone,
}

#[derive(Subcommand)]
enum Commands {
    /// Capture system audio and host a session (macOS 14.2+).
    Host {
        #[arg(long, value_enum, default_value = "system")]
        source: Source,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u32).range(50..=500))]
        buffer_ms: u32,
        #[arg(long, default_value = "0.0.0.0")]
        bind: IpAddr,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long)]
        code: Option<String>,
        #[arg(long)]
        no_discovery: bool,
        #[arg(long, hide = true)]
        headless: bool,
    },
    /// Discover a host by connection code and play its audio.
    Join {
        code: String,
        /// Direct connection fallback when Bonjour is blocked.
        #[arg(long)]
        host: Option<SocketAddr>,
        #[arg(long, hide = true)]
        headless: bool,
    },
    /// Stop the session running on this Mac.
    Leave,
    /// Show the current session and timing statistics.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// List connected output devices, including Bluetooth speakers.
    Devices {
        #[arg(long)]
        json: bool,
    },
    /// Select an output by UID or exact name; use "default" for system output.
    Device { device: String },
    /// Show or set a per-output delay, such as +40ms or -20ms.
    Latency {
        #[arg(allow_hyphen_values = true)]
        offset: Option<String>,
    },
    /// Check macOS support, audio devices and configuration.
    Doctor,
}

fn latency_ms(value: &str) -> Result<i32> {
    let value = value.strip_suffix("ms").unwrap_or(value).parse::<i32>()?;
    ensure!(
        (-500..=500).contains(&value),
        "Latency must be between -500ms and +500ms"
    );
    Ok(value)
}

fn session_missing(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    })
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Host {
            source,
            buffer_ms,
            bind,
            port,
            code,
            no_discovery,
            headless,
        } => {
            session::host(HostOptions {
                bind,
                port,
                code,
                buffer_ms,
                tone: matches!(source, Source::Tone),
                headless,
                discoverable: !no_discovery,
            })
            .await?
        }
        Commands::Join {
            code,
            host,
            headless,
        } => {
            session::join(JoinOptions {
                code,
                host,
                headless,
            })
            .await?
        }
        Commands::Leave => {
            let response = session::local_command(&LocalCommand::Leave).await?;
            ensure!(response.ok, "{}", response.message);
            println!("Leaving session…");
        }
        Commands::Status { json } => {
            let response = session::local_command(&LocalCommand::Status).await?;
            ensure!(response.ok, "{}", response.message);
            if let Some(status) = response.status {
                if json {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!("{}: {}\nCode: {}\nOutput: {}\nBuffer: {}ms | Offset: {:+}ms\nClock offset: {:.3}ms | RTT: {:.3}ms\nClients: {}\nPackets sent: {} | Received: {} | Scheduled: {}\nMissing: {} | Late: {}", status.role, status.state, status.code, status.output, status.buffer_ms, status.latency_ms, status.clock_offset_ms, status.rtt_ms, status.clients.join(", "), status.sent, status.received, status.scheduled, status.missing, status.late);
                }
            }
        }
        Commands::Devices { json } => {
            let devices = audio::devices().await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&devices)?);
            } else {
                for device in devices {
                    println!(
                        "{} {} ({} Hz)\n    {}",
                        if device.is_default { "*" } else { " " },
                        device.name,
                        device.sample_rate,
                        device.uid
                    );
                }
            }
        }
        Commands::Device { device } => {
            let devices = audio::devices().await?;
            let (uid, name) = if device == "default" {
                (
                    None,
                    devices
                        .iter()
                        .find(|d| d.is_default)
                        .map(|d| d.name.clone())
                        .unwrap_or_else(|| "System default".into()),
                )
            } else {
                let matches = devices
                    .iter()
                    .filter(|d| d.uid == device || d.name == device)
                    .collect::<Vec<_>>();
                ensure!(
                    matches.len() == 1,
                    "Device unavailable or name ambiguous. Run oto devices and use its UID."
                );
                (Some(matches[0].uid.clone()), matches[0].name.clone())
            };
            match session::local_command(&LocalCommand::Device {
                uid: uid.clone(),
                name: name.clone(),
            })
            .await
            {
                Ok(response) => ensure!(response.ok, "{}", response.message),
                Err(error) if session_missing(&error) => {
                    let mut settings = Settings::load()?;
                    settings.device = uid;
                    settings.save()?;
                }
                Err(error) => return Err(error),
            }
            println!("Output: {name}");
        }
        Commands::Latency { offset } => {
            if let Some(offset) = offset {
                let ms = latency_ms(&offset)?;
                match session::local_command(&LocalCommand::Latency { ms }).await {
                    Ok(response) => ensure!(response.ok, "{}", response.message),
                    Err(error) if session_missing(&error) => {
                        let mut settings = Settings::load()?;
                        settings.set_latency(ms);
                        settings.save()?;
                    }
                    Err(error) => return Err(error),
                }
                println!("Output delay: {ms:+}ms");
            } else {
                println!("Output delay: {:+}ms", Settings::load()?.latency());
            }
        }
        Commands::Doctor => {
            if !cfg!(target_os = "macos") {
                bail!("Oto currently supports macOS 14.2 or later");
            }
            let version = tokio::process::Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .await?;
            let version = String::from_utf8_lossy(&version.stdout).trim().to_owned();
            let parts = version
                .split('.')
                .filter_map(|s| s.parse::<u32>().ok())
                .collect::<Vec<_>>();
            ensure!(
                parts.first().copied().unwrap_or(0) > 14
                    || (parts.first() == Some(&14) && parts.get(1).copied().unwrap_or(0) >= 2),
                "macOS 14.2+ required; found {version}"
            );
            println!("✓ macOS {version}\n✓ Native audio engine extracted");
            let devices = audio::devices().await?;
            ensure!(!devices.is_empty(), "No output devices found");
            for device in devices {
                println!(
                    "✓ Output: {}{}",
                    device.name,
                    if device.is_default { " (default)" } else { "" }
                );
            }
            Settings::load()?;
            println!("✓ Configuration readable\nSystem capture permission is requested by oto host.\nAllow Oto/your terminal under Privacy & Security → Screen & System Audio Recording.\nAllow incoming connections on the host when macOS asks.");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("oto: {error:#}");
        std::process::exit(1);
    }
}

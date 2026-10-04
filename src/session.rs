use crate::{
    audio::{AudioEngine, Capture, CommandFrame},
    clock::{self, ClockSync, Sample},
    discovery,
    jitter::JitterBuffer,
    protocol::{self, AudioPacket, Control},
    settings::{self, Settings},
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        io::AsRawFd,
    },
    sync::{
        atomic::{AtomicI64, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::BufReader,
    net::{TcpListener, TcpStream, UdpSocket, UnixListener, UnixStream},
    sync::{mpsc, watch, Mutex, RwLock, Semaphore},
    task::JoinSet,
};
use uuid::Uuid;

pub fn hostname() -> String {
    let mut bytes = [0u8; 256];
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } == 0 {
        String::from_utf8_lossy(&bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())])
            .trim_end_matches(".local")
            .to_owned()
    } else {
        "Mac".into()
    }
}
pub fn normalize_code(code: &str) -> Result<String> {
    let code = code.trim().to_ascii_uppercase();
    ensure!(
        code.len() == 5
            && code
                .bytes()
                .all(|b| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(&b)),
        "Connection code must be five characters from 23456789ABCDEFGHJKLMNPQRSTUVWXYZ"
    );
    Ok(code)
}
fn generate_code() -> String {
    let random = Uuid::new_v4();
    random.as_bytes()[..5]
        .iter()
        .map(|b| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ"[(b & 31) as usize] as char)
        .collect()
}

fn host_addresses(bind: IpAddr, port: u16) -> Result<Vec<SocketAddr>> {
    if !bind.is_unspecified() {
        return Ok(vec![SocketAddr::new(bind, port)]);
    }
    let mut addresses = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|interface| !interface.is_loopback() && !interface.is_link_local())
        .map(|interface| interface.ip())
        .filter(|ip| !ip.is_unspecified() && ip.is_ipv4() == bind.is_ipv4())
        .map(|ip| SocketAddr::new(ip, port))
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    Ok(addresses)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    #[serde(default)]
    pub pid: u32,
    pub role: String,
    pub state: String,
    pub code: Option<String>,
    pub hostname: String,
    pub control_port: u16,
    #[serde(default)]
    pub addresses: Vec<SocketAddr>,
    pub clients: Vec<String>,
    pub output: String,
    #[serde(default)]
    pub output_uid: Option<String>,
    #[serde(default = "default_true")]
    pub follow_default: bool,
    pub latency_ms: i32,
    /// Slowest reported physical output delay among current participants.
    #[serde(default)]
    pub target_delay_ms: u32,
    /// Automatic additional software delay for this participant.
    #[serde(default)]
    pub compensation_ms: u32,
    /// Local report last acknowledged by the host (or applied locally as host).
    #[serde(default)]
    pub applied_delay_ms: u32,
    pub buffer_ms: u32,
    pub clock_offset_ms: f64,
    pub rtt_ms: f64,
    pub sent: u64,
    pub received: u64,
    pub scheduled: u64,
    pub missing: u64,
    pub late: u64,
}
fn default_true() -> bool {
    true
}
impl Status {
    fn new(
        role: &str,
        code: Option<String>,
        buffer_ms: u32,
        output: String,
        latency_ms: i32,
    ) -> Self {
        Self {
            pid: std::process::id(),
            role: role.into(),
            state: "starting".into(),
            code,
            hostname: hostname(),
            control_port: 0,
            addresses: vec![],
            clients: vec![],
            output,
            output_uid: None,
            follow_default: true,
            latency_ms,
            target_delay_ms: 0,
            compensation_ms: 0,
            applied_delay_ms: 0,
            buffer_ms,
            clock_offset_ms: 0.,
            rtt_ms: 0.,
            sent: 0,
            received: 0,
            scheduled: 0,
            missing: 0,
            late: 0,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum LocalCommand {
    Status,
    Leave,
    Latency {
        ms: i32,
    },
    AdjustLatency {
        delta_ms: i32,
        output_uid: Option<String>,
    },
    ResetLatency {
        output_uid: Option<String>,
    },
    Device {
        uid: Option<String>,
        name: String,
    },
}
#[derive(Serialize, Deserialize)]
pub struct LocalResponse {
    pub ok: bool,
    pub message: String,
    pub status: Option<Status>,
}

pub async fn local_command(command: &LocalCommand) -> Result<LocalResponse> {
    let stream = UnixStream::connect(settings::directory()?.join("session.sock"))
        .await
        .context("No Oto session is running")?;
    let (read, mut write) = stream.into_split();
    protocol::send(&mut write, command).await?;
    tokio::time::timeout(
        Duration::from_secs(3),
        protocol::receive(&mut BufReader::new(read)),
    )
    .await
    .context("Oto session did not respond")?
}

struct Runtime {
    status: Arc<Mutex<Status>>,
    config: Arc<RwLock<Settings>>,
    stop: watch::Sender<bool>,
    delay_changes: watch::Sender<u64>,
    listener: UnixListener,
    path: std::path::PathBuf,
    _lock: std::fs::File,
}
impl Runtime {
    async fn claim(status: Status) -> Result<Self> {
        let directory = settings::directory()?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("session.lock"))?;
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "An Oto session is already running. Use oto status or oto leave."
        );
        let path = directory.join("session.sock");
        if UnixStream::connect(&path).await.is_ok() {
            bail!("An Oto session is already running. Use oto status or oto leave.");
        }
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            ensure!(
                metadata.file_type().is_socket(),
                "Session socket path is occupied by a non-socket file"
            );
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let (stop, _) = watch::channel(false);
        let (delay_changes, _) = watch::channel(0);
        Ok(Self {
            status: Arc::new(Mutex::new(status)),
            config: Arc::new(RwLock::new(Settings::load()?)),
            stop,
            delay_changes,
            listener,
            path,
            _lock: lock,
        })
    }
    async fn serve(&self, sender: mpsc::Sender<CommandFrame>) -> Result<()> {
        let mut stop = self.stop.subscribe();
        let mut requests = JoinSet::new();
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                Some(_) = requests.join_next(), if !requests.is_empty() => {},
                incoming = self.listener.accept() => {
                    let (stream, _) = incoming?;
                    if requests.len() >= 16 { continue; }
                    let status = self.status.clone();
                    let config = self.config.clone();
                    let shutdown = self.stop.clone();
                    let delay_changes = self.delay_changes.clone();
                    let sender = sender.clone();
                    requests.spawn(async move {
                        let (read, mut write) = stream.into_split();
                        let mut leaving = false;
                        let result: Result<LocalResponse> = async {
                            let command: LocalCommand = tokio::time::timeout(Duration::from_secs(3), protocol::receive(&mut BufReader::new(read))).await??;
                            match command {
                                LocalCommand::Status => {},
                                LocalCommand::Leave => { leaving = true; },
                                command @ (LocalCommand::Latency { .. } | LocalCommand::AdjustLatency { .. } | LocalCommand::ResetLatency { .. }) => {
                                    let mut config = config.write().await;
                                    let mut current = status.lock().await;
                                    let ms = match command {
                                        LocalCommand::Latency { ms } => ms,
                                        LocalCommand::AdjustLatency { delta_ms, output_uid } => {
                                            ensure!(output_uid == current.output_uid, "Output changed; try the adjustment again on the current speaker");
                                            config.latency().checked_add(delta_ms).context("Latency adjustment is too large")?
                                        },
                                        LocalCommand::ResetLatency { output_uid } => {
                                            ensure!(output_uid == current.output_uid, "Output changed; try resetting the current speaker again");
                                            0
                                        },
                                        _ => unreachable!(),
                                    };
                                    ensure!((0..=500).contains(&ms), "Speaker delay must be between 0 and 500 ms");
                                    let mut next = config.clone(); next.set_latency(ms); next.save()?; *config = next;
                                    current.latency_ms = ms;
                                    delay_changes.send_modify(|revision| *revision = revision.wrapping_add(1));
                                },
                                LocalCommand::Device { uid, name: _ } => {
                                    let outputs = crate::audio::devices().await?;
                                    let selected = outputs.iter().find(|device| match &uid { Some(uid) => &device.uid == uid, None => device.is_default }).context("Selected output is unavailable")?;
                                    let mut config = config.write().await;
                                    let mut next = config.clone(); next.device = uid.clone(); next.activate_output(Some(selected.uid.clone()));
                                    let follow_default = uid.is_none();
                                    sender.send(CommandFrame::Device(uid.unwrap_or_default())).await.context("Audio engine stopped")?;
                                    // The engine's acknowledgement commits the active UID and report.
                                    let mut saved = config.clone(); saved.device = next.device; saved.save()?; *config = saved;
                                    drop(config);
                                    tokio::time::timeout(Duration::from_secs(2), async {
                                        loop {
                                            { let current = status.lock().await;
                                              if current.output_uid.as_deref() == Some(selected.uid.as_str()) && current.follow_default == follow_default { break; }
                                            }
                                            tokio::time::sleep(Duration::from_millis(10)).await;
                                        }
                                    }).await.context("Output did not acknowledge the change; check session logs")?;
                                }
                            }
                            Ok(LocalResponse { ok: true, message: "OK".into(), status: Some(status.lock().await.clone()) })
                        }.await;
                        let response = result.unwrap_or_else(|error| LocalResponse { ok: false, message: error.to_string(), status: None });
                        let _ = protocol::send(&mut write, &response).await;
                        if leaving { shutdown.send_replace(true); }
                    });
                }
            }
        }
        Ok(())
    }
    fn install_signals(&self) -> tokio::task::JoinHandle<()> {
        let stop = self.stop.clone();
        tokio::spawn(async move {
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            stop.send_replace(true);
        })
    }
    fn follow_output_changes(&self, engine: &mut AudioEngine) -> AbortOnDrop<()> {
        let status = self.status.clone();
        let config = self.config.clone();
        let delay_changes = self.delay_changes.clone();
        let mut changes = engine
            .device_changes
            .take()
            .expect("audio device event receiver");
        AbortOnDrop(tokio::spawn(async move {
            while let Some(device) = changes.recv().await {
                let mut config = config.write().await;
                config.activate_output(Some(device.uid.clone()));
                let mut status = status.lock().await;
                status.latency_ms = config.latency();
                status.output_uid = Some(device.uid);
                status.follow_default = config.device.is_none();
                if status.output != device.name {
                    eprintln!("✓ Output changed: {}", device.name);
                }
                status.output = device.name;
                delay_changes.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }))
    }
    async fn initialize_output(&self, engine: &AudioEngine) -> Result<()> {
        let mut config = self.config.write().await;
        config.activate_output(engine.device_uid.clone());
        config.save()?;
        let mut status = self.status.lock().await;
        status.output = engine.device.clone();
        status.output_uid = engine.device_uid.clone();
        status.follow_default = config.device.is_none();
        status.latency_ms = config.latency();
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct HostOptions {
    pub bind: IpAddr,
    pub port: u16,
    pub code: Option<String>,
    pub no_code: bool,
    pub buffer_ms: u32,
    pub tone: bool,
    pub headless: bool,
    pub discoverable: bool,
}
struct Peer {
    address: SocketAddr,
    token: Uuid,
    name: String,
    speaker_delay_ms: u32,
}
type Peers = Arc<RwLock<HashMap<Uuid, Peer>>>;

#[derive(Clone)]
struct Coordinator {
    peers: Peers,
    config: Arc<RwLock<Settings>>,
    status: Arc<Mutex<Status>>,
}
struct PeerOutput {
    address: SocketAddr,
    token: Uuid,
    speaker_delay_ms: u32,
}
struct PlaybackPlan {
    local_delay_ms: u32,
    target_delay_ms: u32,
    outputs: Vec<PeerOutput>,
}
impl Coordinator {
    async fn plan(&self) -> PlaybackPlan {
        let config = self.config.read().await;
        let local_delay_ms = config.latency() as u32;
        let peers = self.peers.read().await;
        let target_delay_ms = peers
            .values()
            .map(|peer| peer.speaker_delay_ms)
            .max()
            .unwrap_or(0)
            .max(local_delay_ms);
        let outputs = peers
            .values()
            .map(|peer| PeerOutput {
                address: peer.address,
                token: peer.token,
                speaker_delay_ms: peer.speaker_delay_ms,
            })
            .collect();
        PlaybackPlan {
            local_delay_ms,
            target_delay_ms,
            outputs,
        }
    }
    async fn refresh_clients(&self) {
        let mut names = self
            .peers
            .read()
            .await
            .values()
            .map(|peer| peer.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        self.status.lock().await.clients = names;
    }
}

async fn serve_peer(
    stream: TcpStream,
    code: Option<String>,
    session: Uuid,
    audio_port: u16,
    buffer_ms: u32,
    coordinator: Coordinator,
) -> Result<()> {
    let address = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let hello: Control =
        tokio::time::timeout(Duration::from_secs(5), protocol::receive(&mut reader)).await??;
    let (name, udp_port, speaker_delay_ms) = match hello {
        Control::Hello {
            version,
            code: supplied,
            name,
            udp_port,
            speaker_delay_ms,
        } if version == protocol::VERSION
            && code
                .as_ref()
                .is_none_or(|expected| supplied.as_ref() == Some(expected))
            && udp_port != 0
            && name.len() <= 128
            && speaker_delay_ms <= 500 =>
        {
            (name, udp_port, speaker_delay_ms)
        }
        _ => {
            protocol::send(
                &mut write,
                &Control::Reject {
                    reason: "Wrong code or incompatible protocol".into(),
                },
            )
            .await?;
            bail!("Rejected connection");
        }
    };
    let id = Uuid::new_v4();
    let token = Uuid::new_v4();
    let target_delay_ms = coordinator
        .plan()
        .await
        .target_delay_ms
        .max(speaker_delay_ms);
    protocol::send(
        &mut write,
        &Control::Welcome {
            version: protocol::VERSION,
            session,
            token,
            udp_port: audio_port,
            buffer_ms,
            target_delay_ms,
        },
    )
    .await?;
    coordinator.peers.write().await.insert(
        id,
        Peer {
            address: SocketAddr::new(address.ip(), udp_port),
            token,
            name: name.clone(),
            speaker_delay_ms,
        },
    );
    eprintln!("✓ {name} connected");
    coordinator.refresh_clients().await;
    let result = async {
        loop {
            let request: Control =
                tokio::time::timeout(Duration::from_secs(10), protocol::receive(&mut reader))
                    .await??;
            let t2 = clock::now_ns();
            match request {
                Control::Sync {
                    t1,
                    speaker_delay_ms,
                } => {
                    ensure!(
                        speaker_delay_ms <= 500,
                        "Speaker delay report exceeds 500 ms"
                    );
                    {
                        let mut peers = coordinator.peers.write().await;
                        let peer = peers.get_mut(&id).context("Participant left the session")?;
                        if peer.speaker_delay_ms != speaker_delay_ms {
                            eprintln!("{name} speaker delay: {speaker_delay_ms} ms");
                            peer.speaker_delay_ms = speaker_delay_ms;
                        }
                    }
                    let target_delay_ms = coordinator.plan().await.target_delay_ms;
                    protocol::send(
                        &mut write,
                        &Control::Synced {
                            t1,
                            t2,
                            t3: clock::now_ns(),
                            speaker_delay_ms,
                            target_delay_ms,
                        },
                    )
                    .await?
                }
                _ => bail!("Unexpected session control message"),
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    }
    .await;
    coordinator.peers.write().await.remove(&id);
    coordinator.refresh_clients().await;
    eprintln!("{name} disconnected");
    result
}

pub async fn host(options: HostOptions) -> Result<()> {
    ensure!(
        (50..=500).contains(&options.buffer_ms),
        "Buffer must be between 50 and 500ms"
    );
    ensure!(
        !options.headless || options.tone,
        "--headless requires --source tone"
    );
    ensure!(
        !options.no_code || options.code.is_none(),
        "--no-code cannot be combined with --code"
    );
    let code = if options.no_code {
        None
    } else {
        Some(
            options
                .code
                .as_deref()
                .map(normalize_code)
                .transpose()?
                .unwrap_or_else(generate_code),
        )
    };
    let settings = Settings::load()?;
    let runtime = Runtime::claim(Status::new(
        "host",
        code.clone(),
        options.buffer_ms,
        "starting".into(),
        settings.latency(),
    ))
    .await?;
    let tcp = TcpListener::bind(SocketAddr::new(options.bind, options.port))
        .await
        .with_context(|| {
            format!(
                "Could not bind host port {}. Use --port to choose another port.",
                options.port
            )
        })?;
    let port = tcp.local_addr()?.port();
    let addresses = match host_addresses(options.bind, port) {
        Ok(addresses) => addresses,
        Err(error) => {
            eprintln!("Could not list host IP addresses: {error}");
            vec![]
        }
    };
    let udp = UdpSocket::bind(SocketAddr::new(options.bind, 0)).await?;
    let audio_port = udp.local_addr()?.port();
    let session = Uuid::new_v4();
    let _advertisement = if options.discoverable {
        Some(discovery::Advertisement::publish(code.as_deref(), session, port).context("Could not advertise the session over Bonjour; use --no-discovery for a direct connection")?)
    } else {
        None
    };
    let mut engine =
        AudioEngine::start(!options.tone, settings.device.as_deref(), options.headless).await?;
    runtime.initialize_output(&engine).await?;
    {
        let mut status = runtime.status.lock().await;
        status.output = engine.device.clone();
        status.control_port = port;
        status.addresses = addresses.clone();
        status.state = "streaming".into();
    }
    let _output_changes = runtime.follow_output_changes(&mut engine);
    eprintln!("\n  OTO 🎵\n\nHosting audio session\nCode: {}\nControl port: {port}\nOutput: {}\nBuffer: {}ms", code.as_deref().unwrap_or("not required"), engine.device, options.buffer_ms);
    for address in addresses {
        eprintln!(
            "Host IP: {}\nConnect: oto join{} --host {address}",
            address.ip(),
            code.as_ref()
                .map(|code| format!(" {code}"))
                .unwrap_or_default()
        );
    }
    eprintln!("\nWaiting for devices… (Ctrl-C to stop)");
    let signals = runtime.install_signals();
    let peers: Peers = Arc::new(RwLock::new(HashMap::new()));
    let coordinator = Coordinator {
        peers: peers.clone(),
        config: runtime.config.clone(),
        status: runtime.status.clone(),
    };
    let semaphore = Arc::new(Semaphore::new(16));
    let mut clients = JoinSet::new();
    let mut stop = runtime.stop.subscribe();
    let mut tone_tick = tokio::time::interval(Duration::from_millis(5));
    tone_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0u64;
    let tone_origin = clock::now_ns();
    let mut errors_open = true;
    let result: Result<()> = async {
        let ipc = runtime.serve(engine.sender.clone());
        tokio::pin!(ipc);
        loop {
            let capture = tokio::select! {
                _ = stop.changed() => break,
                result = &mut ipc => { result?; break; },
                error = engine.errors.recv(), if errors_open => { if let Some(error) = error { bail!(error); } errors_open = false; continue; },
                Some(_) = clients.join_next(), if !clients.is_empty() => { continue; },
                accepted = tcp.accept() => {
                    let (stream, _) = accepted?;
                    if let Ok(permit) = semaphore.clone().try_acquire_owned() {
                        let code = code.clone(); let coordinator = coordinator.clone();
                        clients.spawn(async move { let _permit = permit; let _ = serve_peer(stream, code, session, audio_port, options.buffer_ms, coordinator).await; });
                    }
                    continue;
                },
                _ = tone_tick.tick(), if options.tone => {
                    // Use a fixed sample timeline so timer wake-up jitter cannot
                    // introduce overlaps or gaps in the diagnostic tone.
                    let slot = clock::now_ns().saturating_sub(tone_origin).saturating_add(2_500_000) / 5_000_000;
                    let phase = slot * u64::from(protocol::FRAMES);
                    let mut pcm = Vec::with_capacity(protocol::PAYLOAD);
                    for frame in 0..protocol::FRAMES as u64 {
                        let sample = ((phase + frame) as f64 * 440. * std::f64::consts::TAU / protocol::RATE as f64).sin() * 4_000.;
                        let sample = (sample as i16).to_le_bytes(); pcm.extend_from_slice(&sample); pcm.extend_from_slice(&sample);
                    }
                    Capture { timestamp: tone_origin + slot * 5_000_000, pcm }
                },
                captured = engine.captures.recv(), if !options.tone => captured.context("System audio capture stopped")?,
            };
            let plan = coordinator.plan().await;
            // All outputs target the same acoustic time. Feed each device earlier
            // by its own physical delay; every participant retains the full buffer.
            let audible_time = capture.timestamp.saturating_add(u64::from(options.buffer_ms + plan.target_delay_ms) * 1_000_000);
            let local_time = audible_time.saturating_sub(u64::from(plan.local_delay_ms) * 1_000_000);
            let _ = engine.sender.try_send(CommandFrame::Audio { timestamp: local_time, pcm: capture.pcm.clone() });
            let mut sent = 0;
            for output in plan.outputs {
                let timestamp = audible_time.saturating_sub(u64::from(output.speaker_delay_ms) * 1_000_000);
                let packet = AudioPacket { session, token: output.token, sequence, timestamp, pcm: capture.pcm.clone() }.encode()?;
                if udp.send_to(&packet, output.address).await.is_ok() { sent += 1; }
            }
            let mut status = runtime.status.lock().await;
            if status.target_delay_ms != plan.target_delay_ms {
                eprintln!("Session speaker delay target: {} ms; coordinating all outputs", plan.target_delay_ms);
            }
            status.target_delay_ms = plan.target_delay_ms;
            status.applied_delay_ms = plan.local_delay_ms;
            status.compensation_ms = plan.target_delay_ms - plan.local_delay_ms;
            status.sent += sent; status.scheduled += 1;
            sequence = sequence.wrapping_add(1);
        }
        Ok(())
    }.await;
    runtime.stop.send_replace(true);
    clients.abort_all();
    signals.abort();
    engine.stop().await;
    eprintln!("Session stopped.");
    result
}

pub struct JoinOptions {
    pub code: Option<String>,
    pub host: Option<SocketAddr>,
    pub headless: bool,
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn exchange(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    speaker_delay_ms: u32,
) -> Result<(Sample, u32)> {
    let t1 = clock::now_ns();
    protocol::send(
        writer,
        &Control::Sync {
            t1,
            speaker_delay_ms,
        },
    )
    .await?;
    let response: Control = tokio::time::timeout(Duration::from_secs(3), protocol::receive(reader))
        .await
        .context("Clock synchronization timed out")??;
    let t4 = clock::now_ns();
    match response {
        Control::Synced {
            t1: echoed,
            t2,
            t3,
            speaker_delay_ms: accepted,
            target_delay_ms,
        } if echoed == t1
            && accepted == speaker_delay_ms
            && (accepted..=500).contains(&target_delay_ms) =>
        {
            Ok((
                Sample::from_exchange(t1, t2, t3, t4).context("Invalid clock sample")?,
                target_delay_ms,
            ))
        }
        _ => bail!("Invalid clock synchronization response"),
    }
}

async fn client_connection(
    options: &JoinOptions,
    runtime: &Runtime,
    sender: mpsc::Sender<CommandFrame>,
) -> Result<()> {
    runtime.status.lock().await.state = "discovering".into();
    let host = if let Some(address) = options.host {
        address
    } else {
        discovery::find(
            options
                .code
                .as_deref()
                .context("A connection code or --host is required")?,
            Duration::from_secs(5),
        )
        .await?
    };
    let stream = tokio::time::timeout(Duration::from_secs(3), TcpStream::connect(host))
        .await
        .context("Connection timed out")??;
    stream.set_nodelay(true)?;
    let bind = match host.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    let udp = UdpSocket::bind(SocketAddr::new(bind, 0)).await?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut delay_changes = runtime.delay_changes.subscribe();
    let initial_delay_ms = runtime.config.read().await.latency() as u32;
    protocol::send(
        &mut write,
        &Control::Hello {
            version: protocol::VERSION,
            code: options.code.clone(),
            name: hostname(),
            udp_port: udp.local_addr()?.port(),
            speaker_delay_ms: initial_delay_ms,
        },
    )
    .await?;
    let welcome: Control =
        tokio::time::timeout(Duration::from_secs(3), protocol::receive(&mut reader)).await??;
    let (session, token, audio_port, buffer_ms, mut target_delay_ms) = match welcome {
        Control::Welcome {
            version,
            session,
            token,
            udp_port,
            buffer_ms,
            target_delay_ms,
        } if version == protocol::VERSION
            && (50..=500).contains(&buffer_ms)
            && udp_port != 0
            && (initial_delay_ms..=500).contains(&target_delay_ms) =>
        {
            (session, token, udp_port, buffer_ms, target_delay_ms)
        }
        Control::Reject { reason } => bail!("Host rejected the connection: {reason}"),
        _ => bail!("Invalid session handshake"),
    };
    {
        let config = runtime.config.read().await;
        let mut status = runtime.status.lock().await;
        status.buffer_ms = buffer_ms;
        status.latency_ms = config.latency();
        status.target_delay_ms = target_delay_ms;
        status.applied_delay_ms = initial_delay_ms;
        status.compensation_ms = target_delay_ms - initial_delay_ms;
    }
    udp.connect(SocketAddr::new(host.ip(), audio_port)).await?;
    runtime.status.lock().await.state = "synchronizing".into();
    let mut sync = ClockSync::default();
    let mut best = Sample {
        offset_ns: 0,
        rtt_ns: u64::MAX,
    };
    let mut applied_delay_ms = initial_delay_ms;
    for _ in 0..12 {
        applied_delay_ms = runtime.config.read().await.latency() as u32;
        let (sample, target) = exchange(&mut reader, &mut write, applied_delay_ms).await?;
        best = sync.observe(sample);
        target_delay_ms = target;
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    let offset = Arc::new(AtomicI64::new(best.offset_ns));
    let rtt = Arc::new(AtomicU64::new(best.rtt_ns));
    {
        let mut status = runtime.status.lock().await;
        status.state = "playing".into();
        status.buffer_ms = buffer_ms;
        status.target_delay_ms = target_delay_ms;
        status.applied_delay_ms = applied_delay_ms;
        status.compensation_ms = target_delay_ms - applied_delay_ms;
        status.control_port = host.port();
        status.addresses = vec![host];
        status.clock_offset_ms = best.offset_ns as f64 / 1_000_000.;
        status.rtt_ms = best.rtt_ns as f64 / 1_000_000.;
    }
    eprintln!(
        "✓ Connected to {host}\n✓ Clock synchronized (RTT {:.2}ms)\nPlaying…",
        best.rtt_ns as f64 / 1_000_000.
    );
    let clock_offset = offset.clone();
    let clock_rtt = rtt.clone();
    let config = runtime.config.clone();
    let status = runtime.status.clone();
    let mut clock_task = AbortOnDrop(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                changed = delay_changes.changed() => { changed.context("Session delay controls stopped")?; },
            }
            let reported = config.read().await.latency() as u32;
            let (sample, target) = exchange(&mut reader, &mut write, reported).await?;
            let best = sync.observe(sample);
            let previous = clock_offset.load(Ordering::Relaxed);
            // At most 0.2ms per update avoids abrupt jumps in queued audio.
            clock_offset.store(
                previous.saturating_add(
                    best.offset_ns
                        .saturating_sub(previous)
                        .clamp(-200_000, 200_000),
                ),
                Ordering::Relaxed,
            );
            clock_rtt.store(best.rtt_ns, Ordering::Relaxed);
            let mut status = status.lock().await;
            if status.target_delay_ms != target || status.applied_delay_ms != reported {
                eprintln!("Speaker delay reported: {reported} ms; automatic added delay: {} ms; session target: {target} ms", target - reported);
            }
            status.target_delay_ms = target;
            status.applied_delay_ms = reported;
            status.compensation_ms = target - reported;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    }));
    let mut jitter = JitterBuffer::default();
    let mut bytes = [0u8; 2048];
    let mut tick = tokio::time::interval(Duration::from_millis(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_audio = tokio::time::Instant::now();
    let mut stop = runtime.stop.subscribe();
    let result = async {
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                result = &mut clock_task.0 => { result??; bail!("Host clock connection stopped"); },
                received = udp.recv(&mut bytes) => {
                    let count = received?;
                    if let Ok(mut packet) = AudioPacket::decode(&bytes[..count], session, token) {
                        // The host has already applied this output's compensation.
                        packet.timestamp = clock::shifted(packet.timestamp, offset.load(Ordering::Relaxed));
                        jitter.push(packet, clock::now_ns());
                        last_audio = tokio::time::Instant::now();
                        runtime.status.lock().await.received += 1;
                    }
                },
                _ = tick.tick() => {
                    ensure!(last_audio.elapsed() < Duration::from_secs(5), "Audio stream interrupted");
                    let mut scheduled = 0;
                    for packet in jitter.ready(clock::now_ns()) {
                        if sender.try_send(CommandFrame::Audio { timestamp: packet.timestamp, pcm: packet.pcm }).is_ok() { scheduled += 1; }
                    }
                    let mut status = runtime.status.lock().await;
                    status.scheduled += scheduled; status.missing = jitter.missing; status.late = jitter.late;
                    status.clock_offset_ms = offset.load(Ordering::Relaxed) as f64 / 1_000_000.;
                    status.rtt_ms = rtt.load(Ordering::Relaxed) as f64 / 1_000_000.;
                }
            }
        }
        Ok(())
    }.await;
    clock_task.0.abort();
    result
}

pub async fn join(mut options: JoinOptions) -> Result<()> {
    options.code = options.code.as_deref().map(normalize_code).transpose()?;
    ensure!(
        options.code.is_some() || options.host.is_some(),
        "A connection code or --host is required"
    );
    let settings = Settings::load()?;
    let runtime = Runtime::claim(Status::new(
        "client",
        options.code.clone(),
        200,
        "starting".into(),
        settings.latency(),
    ))
    .await?;
    let mut engine =
        AudioEngine::start(false, settings.device.as_deref(), options.headless).await?;
    runtime.initialize_output(&engine).await?;
    let _output_changes = runtime.follow_output_changes(&mut engine);
    eprintln!(
        "\n  OTO 🎵\n\nJoining {}\nOutput: {}\nSearching for host… (Ctrl-C to leave)",
        options
            .code
            .clone()
            .unwrap_or_else(|| options.host.unwrap().to_string()),
        engine.device
    );
    let signals = runtime.install_signals();
    let mut stop = runtime.stop.subscribe();
    let mut errors_open = true;
    let mut retry = 1u64;
    let result: Result<()> = async {
        let ipc = runtime.serve(engine.sender.clone());
        tokio::pin!(ipc);
        loop {
            let connected = client_connection(&options, &runtime, engine.sender.clone());
            tokio::pin!(connected);
            let error = tokio::select! {
                _ = stop.changed() => break,
                result = &mut ipc => { result?; break; },
                error = engine.errors.recv(), if errors_open => { if let Some(error) = error { bail!(error); } errors_open = false; continue; },
                result = &mut connected => match result { Ok(()) => break, Err(error) => error },
            };
            runtime.status.lock().await.state = "reconnecting".into();
            eprintln!("{error:#}\nReconnecting in {retry}s…");
            tokio::select! {
                _ = stop.changed() => break,
                result = &mut ipc => { result?; break; },
                error = engine.errors.recv(), if errors_open => { if let Some(error) = error { bail!(error); } errors_open = false; },
                _ = tokio::time::sleep(Duration::from_secs(retry)) => {}
            }
            retry = (retry * 2).min(5);
        }
        Ok(())
    }.await;
    runtime.stop.send_replace(true);
    signals.abort();
    engine.stop().await;
    eprintln!("Left session.");
    result
}

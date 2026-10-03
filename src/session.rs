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
    pub role: String,
    pub state: String,
    pub code: Option<String>,
    pub hostname: String,
    pub control_port: u16,
    #[serde(default)]
    pub addresses: Vec<SocketAddr>,
    pub clients: Vec<String>,
    pub output: String,
    pub latency_ms: i32,
    pub buffer_ms: u32,
    pub clock_offset_ms: f64,
    pub rtt_ms: f64,
    pub sent: u64,
    pub received: u64,
    pub scheduled: u64,
    pub missing: u64,
    pub late: u64,
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
            role: role.into(),
            state: "starting".into(),
            code,
            hostname: hostname(),
            control_port: 0,
            addresses: vec![],
            clients: vec![],
            output,
            latency_ms,
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
    Latency { ms: i32 },
    Device { uid: Option<String>, name: String },
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
        Ok(Self {
            status: Arc::new(Mutex::new(status)),
            config: Arc::new(RwLock::new(Settings::load()?)),
            stop,
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
                    let sender = sender.clone();
                    requests.spawn(async move {
                        let (read, mut write) = stream.into_split();
                        let mut leaving = false;
                        let result: Result<LocalResponse> = async {
                            let command: LocalCommand = tokio::time::timeout(Duration::from_secs(3), protocol::receive(&mut BufReader::new(read))).await??;
                            match command {
                                LocalCommand::Status => {},
                                LocalCommand::Leave => { leaving = true; },
                                LocalCommand::Latency { ms } => {
                                    ensure!((-500..=500).contains(&ms), "Latency must be between -500ms and +500ms");
                                    ensure!(i64::from(status.lock().await.buffer_ms) + i64::from(ms) >= 50, "Increase --buffer-ms before applying this negative offset (at least 50ms must remain)");
                                    let mut config = config.write().await;
                                    config.set_latency(ms); config.save()?;
                                    status.lock().await.latency_ms = ms;
                                },
                                LocalCommand::Device { uid, name: _ } => {
                                    let mut config = config.write().await;
                                    let mut next = config.clone(); next.device = uid.clone();
                                    ensure!(i64::from(status.lock().await.buffer_ms) + i64::from(next.latency()) >= 50, "Saved device offset leaves less than 50ms of buffering");
                                    sender.send(CommandFrame::Device(uid.unwrap_or_default())).await.context("Audio engine stopped")?;
                                    next.save()?; *config = next;
                                    let mut status = status.lock().await;
                                    status.latency_ms = config.latency();
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
        let mut changes = engine
            .device_changes
            .take()
            .expect("audio device event receiver");
        AbortOnDrop(tokio::spawn(async move {
            while let Some(device) = changes.recv().await {
                let mut status = status.lock().await;
                if status.output != device.name {
                    eprintln!("✓ Output changed: {}", device.name);
                }
                status.output = device.name;
            }
        }))
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
}
type Peers = Arc<RwLock<HashMap<Uuid, Peer>>>;

async fn serve_peer(
    stream: TcpStream,
    code: Option<String>,
    session: Uuid,
    audio_port: u16,
    buffer_ms: u32,
    peers: Peers,
    status: Arc<Mutex<Status>>,
) -> Result<()> {
    let address = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let hello: Control =
        tokio::time::timeout(Duration::from_secs(5), protocol::receive(&mut reader)).await??;
    let (name, udp_port) = match hello {
        Control::Hello {
            version,
            code: supplied,
            name,
            udp_port,
        } if version == protocol::VERSION
            && code
                .as_ref()
                .is_none_or(|expected| supplied.as_ref() == Some(expected))
            && udp_port != 0
            && name.len() <= 128 =>
        {
            (name, udp_port)
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
    protocol::send(
        &mut write,
        &Control::Welcome {
            version: protocol::VERSION,
            session,
            token,
            udp_port: audio_port,
            buffer_ms,
        },
    )
    .await?;
    peers.write().await.insert(
        id,
        Peer {
            address: SocketAddr::new(address.ip(), udp_port),
            token,
            name: name.clone(),
        },
    );
    eprintln!("✓ {name} connected");
    status.lock().await.clients = peers
        .read()
        .await
        .values()
        .map(|p| p.name.clone())
        .collect();
    let result = async {
        loop {
            let request: Control =
                tokio::time::timeout(Duration::from_secs(10), protocol::receive(&mut reader))
                    .await??;
            let t2 = clock::now_ns();
            match request {
                Control::Sync { t1 } => {
                    protocol::send(
                        &mut write,
                        &Control::Synced {
                            t1,
                            t2,
                            t3: clock::now_ns(),
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
    peers.write().await.remove(&id);
    status.lock().await.clients = peers
        .read()
        .await
        .values()
        .map(|p| p.name.clone())
        .collect();
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
    ensure!(
        i64::from(options.buffer_ms) + i64::from(settings.latency()) >= 50,
        "Saved latency offset requires a larger --buffer-ms"
    );
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
                        let code = code.clone(); let peers = peers.clone(); let status = runtime.status.clone();
                        clients.spawn(async move { let _permit = permit; let _ = serve_peer(stream, code, session, audio_port, options.buffer_ms, peers, status).await; });
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
            let timestamp = capture.timestamp.saturating_add(u64::from(options.buffer_ms) * 1_000_000);
            let latency = runtime.config.read().await.latency();
            let _ = engine.sender.try_send(CommandFrame::Audio { timestamp: clock::shifted(timestamp, i64::from(latency) * 1_000_000), pcm: capture.pcm.clone() });
            let targets = peers.read().await.values().map(|p| (p.address, p.token)).collect::<Vec<_>>();
            let mut sent = 0;
            for (address, token) in targets {
                let packet = AudioPacket { session, token, sequence, timestamp, pcm: capture.pcm.clone() }.encode()?;
                if udp.send_to(&packet, address).await.is_ok() { sent += 1; }
            }
            let mut status = runtime.status.lock().await;
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
) -> Result<Sample> {
    let t1 = clock::now_ns();
    protocol::send(writer, &Control::Sync { t1 }).await?;
    let response: Control = tokio::time::timeout(Duration::from_secs(3), protocol::receive(reader))
        .await
        .context("Clock synchronization timed out")??;
    let t4 = clock::now_ns();
    match response {
        Control::Synced { t1: echoed, t2, t3 } if echoed == t1 => {
            Sample::from_exchange(t1, t2, t3, t4).context("Invalid clock sample")
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
    protocol::send(
        &mut write,
        &Control::Hello {
            version: protocol::VERSION,
            code: options.code.clone(),
            name: hostname(),
            udp_port: udp.local_addr()?.port(),
        },
    )
    .await?;
    let welcome: Control =
        tokio::time::timeout(Duration::from_secs(3), protocol::receive(&mut reader)).await??;
    let (session, token, audio_port, buffer_ms) = match welcome {
        Control::Welcome {
            version,
            session,
            token,
            udp_port,
            buffer_ms,
        } if version == protocol::VERSION && (50..=500).contains(&buffer_ms) && udp_port != 0 => {
            (session, token, udp_port, buffer_ms)
        }
        Control::Reject { reason } => bail!("Host rejected the connection: {reason}"),
        _ => bail!("Invalid session handshake"),
    };
    ensure!(
        i64::from(buffer_ms) + i64::from(runtime.config.read().await.latency()) >= 50,
        "Saved latency offset leaves less than 50ms of buffering"
    );
    udp.connect(SocketAddr::new(host.ip(), audio_port)).await?;
    runtime.status.lock().await.state = "synchronizing".into();
    let mut sync = ClockSync::default();
    let mut best = Sample {
        offset_ns: 0,
        rtt_ns: u64::MAX,
    };
    for _ in 0..12 {
        best = sync.observe(exchange(&mut reader, &mut write).await?);
        tokio::time::sleep(Duration::from_millis(3)).await;
    }
    let offset = Arc::new(AtomicI64::new(best.offset_ns));
    let rtt = Arc::new(AtomicU64::new(best.rtt_ns));
    {
        let mut status = runtime.status.lock().await;
        status.state = "playing".into();
        status.buffer_ms = buffer_ms;
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
    let mut clock_task = AbortOnDrop(tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let best = sync.observe(exchange(&mut reader, &mut write).await?);
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
                        let latency = runtime.config.read().await.latency();
                        packet.timestamp = clock::shifted(packet.timestamp, offset.load(Ordering::Relaxed).saturating_add(i64::from(latency) * 1_000_000));
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
    runtime.status.lock().await.output = engine.device.clone();
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

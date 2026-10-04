use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn command(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oto"));
    command.env("OTO_CONFIG_DIR", directory);
    command
}
fn start(directory: &Path, args: &[&str]) -> Process {
    Process(
        command(directory)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
fn start_logged(directory: &Path, args: &[&str]) -> Process {
    let log = std::fs::File::create(directory.join("session.log")).unwrap();
    Process(
        command(directory)
            .args(args)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
}
fn status(directory: &Path) -> Option<Value> {
    let output = command(directory)
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}
fn wait_for(directory: &Path, predicate: impl Fn(&Value) -> bool) -> Value {
    let start = Instant::now();
    loop {
        if let Some(status) = status(directory) {
            if predicate(&status) {
                return status;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "Timed out waiting for session state: {:?}",
            status(directory)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn success(directory: &Path, args: &[&str]) {
    let output = command(directory).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn multi_client_session_controls_and_reconnection() {
    let host = TempDir::new().unwrap();
    let client = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let mut server = start_logged(
        host.path(),
        &[
            "host",
            "--source",
            "tone",
            "--headless",
            "--no-discovery",
            "--bind",
            "127.0.0.1",
            "--port",
            "0",
            "--code",
            "AB234",
        ],
    );
    let host_status = wait_for(host.path(), |s| s["state"] == "streaming");
    let port = host_status["control_port"].as_u64().unwrap();
    let address = format!("127.0.0.1:{port}");
    assert_eq!(host_status["addresses"], serde_json::json!([address]));
    let log = std::fs::read_to_string(host.path().join("session.log")).unwrap();
    assert!(log.contains("Host IP: 127.0.0.1"));
    assert!(log.contains(&format!("Connect: oto join AB234 --host {address}")));
    let _client = start(
        client.path(),
        &["join", "ab234", "--host", &address, "--headless"],
    );
    let _second = start(
        second.path(),
        &["join", "AB234", "--host", &address, "--headless"],
    );
    let stats = wait_for(client.path(), |s| {
        s["scheduled"].as_u64().unwrap_or(0) >= 100
    });
    assert!(stats["received"].as_u64().unwrap() >= 100);
    assert!(stats["rtt_ms"].as_f64().unwrap() < 100.);
    assert!(stats["late"].as_u64().unwrap() < 10);
    wait_for(second.path(), |s| {
        s["scheduled"].as_u64().unwrap_or(0) >= 100
    });
    wait_for(host.path(), |s| s["clients"].as_array().unwrap().len() == 2);

    // Connection codes authorize the handshake; a different code is rejected.
    let mut tcp = TcpStream::connect(&address).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    writeln!(tcp, "{}", serde_json::json!({"type":"hello", "version":oto::protocol::VERSION, "code":"ZZZZZ", "name":"intruder", "udp_port":9999, "speaker_delay_ms":0})).unwrap();
    let mut line = String::new();
    BufReader::new(tcp).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["type"],
        "reject"
    );
    let mut tcp = TcpStream::connect(&address).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    writeln!(
        tcp,
        "{}",
        serde_json::json!({"type":"hello", "version":oto::protocol::VERSION, "name":"no-code", "udp_port":9999, "speaker_delay_ms":0})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(tcp).read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["type"],
        "reject"
    );

    success(client.path(), &["latency", "+40ms"]);
    assert_eq!(status(client.path()).unwrap()["latency_ms"], 40);
    assert!(!command(client.path())
        .args(["latency", "-500ms"])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(status(client.path()).unwrap()["latency_ms"], 40);
    assert!(!command(host.path())
        .args(["host", "--source", "tone", "--headless", "--no-discovery"])
        .output()
        .unwrap()
        .status
        .success());

    success(host.path(), &["leave"]);
    assert!(server.0.wait().unwrap().success());
    wait_for(client.path(), |s| {
        s["state"] == "reconnecting" || s["state"] == "discovering"
    });
    // Local controls continue to work during retry delays.
    success(second.path(), &["leave"]);

    let _server = start(
        host.path(),
        &[
            "host",
            "--source",
            "tone",
            "--headless",
            "--no-discovery",
            "--bind",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--code",
            "AB234",
        ],
    );
    wait_for(host.path(), |s| s["state"] == "streaming");
    let previous = status(client.path()).unwrap()["scheduled"]
        .as_u64()
        .unwrap();
    wait_for(client.path(), |s| {
        s["state"] == "playing" && s["scheduled"].as_u64().unwrap_or(0) > previous + 50
    });
    success(client.path(), &["leave"]);
    success(host.path(), &["leave"]);
}

#[test]
fn bonjour_discovers_a_code_without_an_ip() {
    let directory = TempDir::new().unwrap();
    let _server = start(
        directory.path(),
        &[
            "host",
            "--source",
            "tone",
            "--headless",
            "--port",
            "0",
            "--code",
            "BC345",
        ],
    );
    let expected = wait_for(directory.path(), |s| s["state"] == "streaming")["control_port"]
        .as_u64()
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let discovered = runtime
        .block_on(oto::discovery::find("BC345", Duration::from_secs(5)))
        .unwrap();
    assert_eq!(discovered.port(), expected as u16);
    assert!(TcpStream::connect_timeout(&discovered, Duration::from_secs(2)).is_ok());
    success(directory.path(), &["leave"]);
}

#[test]
fn direct_session_without_a_code() {
    let host = TempDir::new().unwrap();
    let client = TempDir::new().unwrap();
    let _server = start_logged(
        host.path(),
        &[
            "host",
            "--source",
            "tone",
            "--headless",
            "--no-discovery",
            "--bind",
            "127.0.0.1",
            "--port",
            "0",
            "--no-code",
        ],
    );
    let stats = wait_for(host.path(), |s| s["state"] == "streaming");
    assert!(stats["code"].is_null());
    let address = format!("127.0.0.1:{}", stats["control_port"].as_u64().unwrap());
    assert_eq!(stats["addresses"], serde_json::json!([address]));
    let log = std::fs::read_to_string(host.path().join("session.log")).unwrap();
    assert!(log.contains("Host IP: 127.0.0.1"));
    assert!(log.contains(&format!("Connect: oto join --host {address}")));
    let _client = start(client.path(), &["join", "--host", &address, "--headless"]);
    let stats = wait_for(client.path(), |s| s["scheduled"].as_u64().unwrap_or(0) > 50);
    assert!(stats["code"].is_null());
    assert!(stats["received"].as_u64().unwrap() > 50);
    assert_eq!(stats["state"], "playing");
    success(client.path(), &["leave"]);
    success(host.path(), &["leave"]);
}

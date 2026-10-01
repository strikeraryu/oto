use oto::audio;
use std::{process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
#[ignore = "requires access to macOS Core Audio output devices"]
async fn native_playback_switches_output_and_cleans_up_on_eof() {
    let directory = tempfile::TempDir::new().unwrap();
    std::env::set_var("OTO_CONFIG_DIR", directory.path());
    let mut child = tokio::process::Command::new(audio::helper_path().unwrap())
        .arg("play")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    async fn read_device(stdout: &mut tokio::process::ChildStdout) -> audio::Device {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut header = [0u8; 16];
            stdout.read_exact(&mut header).await.unwrap();
            assert_eq!(u32::from_le_bytes(header[..4].try_into().unwrap()), 11);
            let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            assert!(length < 4096);
            let mut payload = vec![0u8; length];
            stdout.read_exact(&mut payload).await.unwrap();
            serde_json::from_slice(&payload).unwrap()
        })
        .await
        .unwrap()
    }
    let device = read_device(&mut stdout).await;
    assert!(device.sample_rate > 0.);
    assert!(!device.uid.is_empty());
    // Pin the currently connected device, then restore automatic default
    // following. Both commands must acknowledge the actual native output.
    for uid in [device.uid.as_bytes(), b""] {
        let mut command = Vec::new();
        command.extend_from_slice(&2u32.to_le_bytes());
        command.extend_from_slice(&(uid.len() as u32).to_le_bytes());
        command.extend_from_slice(&0u64.to_le_bytes());
        command.extend_from_slice(uid);
        stdin.write_all(&command).await.unwrap();
        assert_eq!(read_device(&mut stdout).await.uid, device.uid);
    }
    drop(stdin);
    assert!(tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}

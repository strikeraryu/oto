use oto::audio;
use std::{process::Stdio, time::Duration};

#[tokio::test]
#[ignore = "requires access to macOS Core Audio output devices"]
async fn native_playback_initializes_and_cleans_up_on_eof() {
    let directory = tempfile::TempDir::new().unwrap();
    std::env::set_var("OTO_CONFIG_DIR", directory.path());
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(audio::helper_path().unwrap())
            .arg("play")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() >= 16);
    assert_eq!(
        u32::from_le_bytes(output.stdout[..4].try_into().unwrap()),
        11
    );
    let device: audio::Device = serde_json::from_slice(&output.stdout[16..]).unwrap();
    assert!(device.sample_rate > 0.);
    assert!(!device.uid.is_empty());
}

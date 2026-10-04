use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const VERSION: u8 = 2;
pub const DEFAULT_PORT: u16 = 47670;
pub const RATE: u32 = 48_000;
pub const CHANNELS: u8 = 2;
pub const FRAMES: u16 = 240; // 5 ms; 1,024 bytes including header, below LAN MTU.
pub const HEADER: usize = 64;
pub const PAYLOAD: usize = FRAMES as usize * CHANNELS as usize * 2;
pub const SERVICE: &str = "_oto._tcp.local.";

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control {
    Hello {
        version: u8,
        #[serde(default)]
        code: Option<String>,
        name: String,
        udp_port: u16,
        #[serde(default)]
        speaker_delay_ms: u32,
    },
    Welcome {
        version: u8,
        session: Uuid,
        token: Uuid,
        udp_port: u16,
        buffer_ms: u32,
        target_delay_ms: u32,
    },
    Sync {
        t1: u64,
        speaker_delay_ms: u32,
    },
    Synced {
        t1: u64,
        t2: u64,
        t3: u64,
        speaker_delay_ms: u32,
        target_delay_ms: u32,
    },
    Reject {
        reason: String,
    },
}

pub async fn send<T: Serialize, W: AsyncWrite + Unpin>(writer: &mut W, value: &T) -> Result<()> {
    let mut data = serde_json::to_vec(value)?;
    ensure!(data.len() < 4096, "Control message too large");
    data.push(b'\n');
    writer.write_all(&data).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn receive<T: for<'de> Deserialize<'de>, R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<T> {
    let mut data = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        ensure!(!available.is_empty(), "Connection closed");
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + 1)
            .unwrap_or(available.len());
        ensure!(data.len() + count <= 4096, "Control message exceeds 4 KiB");
        let done = available[count - 1] == b'\n';
        data.extend_from_slice(&available[..count]);
        reader.consume(count);
        if done {
            break;
        }
    }
    serde_json::from_slice(&data).context("Invalid control message")
}

#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub session: Uuid,
    pub token: Uuid,
    pub sequence: u64,
    /// Hardware playback time in host monotonic nanoseconds, already compensated
    /// for this receiver's reported speaker delay.
    pub timestamp: u64,
    pub pcm: Vec<u8>,
}
impl AudioPacket {
    pub fn encode(&self) -> Result<Vec<u8>> {
        ensure!(
            self.pcm.len() == PAYLOAD,
            "PCM must contain 240 stereo i16 frames"
        );
        let mut bytes = Vec::with_capacity(HEADER + PAYLOAD);
        bytes.extend_from_slice(b"OTO1");
        bytes.extend_from_slice(&[VERSION, 0]);
        bytes.extend_from_slice(&(HEADER as u16).to_be_bytes());
        bytes.extend_from_slice(self.session.as_bytes());
        bytes.extend_from_slice(self.token.as_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.extend_from_slice(&FRAMES.to_be_bytes());
        bytes.extend_from_slice(&[CHANNELS, 1]); // format 1: little-endian signed 16 bit
        bytes.extend_from_slice(&RATE.to_be_bytes());
        bytes.extend_from_slice(&self.pcm);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], session: Uuid, token: Uuid) -> Result<Self> {
        ensure!(
            bytes.len() == HEADER + PAYLOAD,
            "Invalid audio datagram size"
        );
        ensure!(
            &bytes[..4] == b"OTO1" && bytes[4] == VERSION && bytes[5] == 0,
            "Unsupported audio protocol"
        );
        ensure!(
            u16::from_be_bytes(bytes[6..8].try_into()?) as usize == HEADER,
            "Invalid header"
        );
        if bytes[8..24] != *session.as_bytes() || bytes[24..40] != *token.as_bytes() {
            bail!("Wrong session or token");
        }
        ensure!(
            u16::from_be_bytes(bytes[56..58].try_into()?) == FRAMES
                && bytes[58] == CHANNELS
                && bytes[59] == 1
                && u32::from_be_bytes(bytes[60..64].try_into()?) == RATE,
            "Unsupported PCM format"
        );
        Ok(Self {
            session,
            token,
            sequence: u64::from_be_bytes(bytes[40..48].try_into()?),
            timestamp: u64::from_be_bytes(bytes[48..56].try_into()?),
            pcm: bytes[HEADER..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hello_supports_existing_codes_and_no_code() {
        for (json, expected) in [
            (
                r#"{"type":"hello","version":1,"code":"AB234","name":"Mac","udp_port":9000}"#,
                Some("AB234"),
            ),
            (
                r#"{"type":"hello","version":1,"name":"Mac","udp_port":9000}"#,
                None,
            ),
            (
                r#"{"type":"hello","version":1,"code":null,"name":"Mac","udp_port":9000}"#,
                None,
            ),
        ] {
            let Control::Hello { code, .. } = serde_json::from_str(json).unwrap() else {
                panic!("expected hello")
            };
            assert_eq!(code.as_deref(), expected);
        }
    }
    #[test]
    fn validates_wire_format_and_session() {
        let p = AudioPacket {
            session: Uuid::new_v4(),
            token: Uuid::new_v4(),
            sequence: u64::MAX,
            timestamp: 123,
            pcm: vec![7; PAYLOAD],
        };
        let encoded = p.encode().unwrap();
        assert!(encoded.len() < 1200);
        assert_eq!(
            AudioPacket::decode(&encoded, p.session, p.token)
                .unwrap()
                .sequence,
            u64::MAX
        );
        assert!(AudioPacket::decode(&encoded, p.session, Uuid::new_v4()).is_err());
        for end in 0..encoded.len() {
            assert!(AudioPacket::decode(&encoded[..end], p.session, p.token).is_err());
        }
        let mut bad = encoded;
        bad[58] = 1;
        assert!(AudioPacket::decode(&bad, p.session, p.token).is_err());
    }
    #[tokio::test]
    async fn control_reader_is_bounded() {
        let bytes = b"xxxxxxxxxxxxxxxx".repeat(300);
        let mut reader = tokio::io::BufReader::new(&bytes[..]);
        assert!(receive::<Control, _>(&mut reader).await.is_err());
    }
}

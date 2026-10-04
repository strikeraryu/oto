use crate::protocol::AudioPacket;
use std::collections::BTreeMap;

/// Holds at most 500 ms, discards duplicates and late packets, and releases in
/// presentation order, waiting at most 10 ms for a sequence gap. The native
/// timestamp ring holds future samples until their hardware playback deadline.
#[derive(Default)]
pub struct JitterBuffer {
    packets: BTreeMap<u64, (AudioPacket, u64)>,
    delivered: Option<u64>,
    pub late: u64,
    pub missing: u64,
}
impl JitterBuffer {
    pub fn push(&mut self, packet: AudioPacket, now: u64) {
        if self.delivered.is_some_and(|n| packet.sequence <= n) {
            return;
        }
        if packet.timestamp.saturating_add(5_000_000) < now {
            self.late += 1;
            return;
        }
        // A 500ms network buffer plus 500ms speaker compensation can reach 1s;
        // leave margin for capture callback timestamps and clock estimation.
        if packet.timestamp > now.saturating_add(1_500_000_000) {
            return;
        }
        if self.packets.len() >= 100 && !self.packets.contains_key(&packet.sequence) {
            return;
        }
        self.packets.entry(packet.sequence).or_insert((packet, now));
    }
    pub fn ready(&mut self, now: u64) -> Vec<AudioPacket> {
        let mut output = Vec::new();
        while let Some((&seq, (_, arrived))) = self.packets.first_key_value() {
            let expected = self.delivered.map(|n| n.wrapping_add(1));
            if expected != Some(seq) && now < arrived.saturating_add(10_000_000) {
                break;
            }
            let (packet, _) = self.packets.remove(&seq).unwrap();
            if let Some(previous) = self.delivered {
                self.missing += seq.saturating_sub(previous).saturating_sub(1);
            }
            self.delivered = Some(seq);
            if packet.timestamp.saturating_add(5_000_000) < now {
                self.late += 1;
            } else {
                output.push(packet);
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    fn packet(sequence: u64, timestamp: u64) -> AudioPacket {
        AudioPacket {
            sequence,
            timestamp,
            session: Uuid::nil(),
            token: Uuid::nil(),
            pcm: vec![],
        }
    }
    #[test]
    fn reorder_duplicate_loss_and_late_packets() {
        let mut b = JitterBuffer::default();
        b.push(packet(2, 110_000_000), 0);
        b.push(packet(0, 100_000_000), 0);
        b.push(packet(0, 100_000_000), 0);
        assert!(b.ready(5_000_000).is_empty());
        let packets = b.ready(15_000_000);
        assert_eq!(
            packets.iter().map(|p| p.sequence).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(b.missing, 1);
        b.push(packet(1, 105_000_000), 110_000_000);
        b.push(packet(3, 100_000_000), 200_000_000);
        assert_eq!(b.late, 1);
    }
}

Updated with **Oto** as the project name and tightened around the actual MVP.

# Oto — Product Requirements Document (PRD)

## 1. Overview

**Oto** is a local-network CLI that synchronizes audio playback across multiple laptops.

The primary use case is:

```text
                 Local Wi-Fi
              ┌───────────────┐
              │               │
          Laptop A        Laptop B
            Host            Client
              │               │
         Bluetooth        Bluetooth
              │               │
          Speaker A      Speaker B
             🔊              🔊
```

The goal is for both speakers to produce the same audio at nearly the same physical time, minimizing audible echo.

---

## 2. Problem

Playing the same audio independently on two laptops causes:

* Different playback start times
* Network-independent clock differences
* Different audio-buffering delays
* Different Bluetooth latency
* Drift over time

Oto solves this by making one laptop the **audio source and timing authority**, while other laptops synchronize their playback against it.

---

# 3. MVP

### Supported

* macOS
* 2+ laptops
* Same local Wi-Fi network
* Host/client architecture
* Bluetooth or built-in speakers
* Terminal-only interface
* Automatic host discovery
* Short connection code
* Real-time audio streaming
* Timestamped audio
* Clock synchronization
* Jitter buffering
* Scheduled playback
* Client reconnection

### Not initially supported

* Internet streaming
* Mobile devices
* Spotify/Apple Music integration
* Remote music controls
* Cloud infrastructure
* Automatic Bluetooth latency measurement
* Cross-network connections

---

# 4. User Experience

## Host

```bash
oto host
```

```text
       OTO 🎵

  Hosting audio session

  Code: 7K4P9

  Waiting for devices...
```

When a client joins:

```text
✓ MacBook-Pro connected
✓ Clock synchronized
✓ Output: JBL Flip 6

Streaming...
```

## Client

```bash
oto join 7K4P9
```

```text
       OTO 🎵

Searching for host...

✓ Found host
✓ Connected
✓ Synchronizing clock
✓ Buffer ready

Output: JBL Flip 6

Playing...
```

No IP address should normally be required from the user.

---

# 5. CLI

### Core

```bash
oto host
oto join <code>
oto leave
oto status
```

### Audio

```bash
oto devices
oto device <device>
```

### Diagnostics

```bash
oto status
oto latency
oto doctor
```

Potential future:

```bash
oto calibrate
oto clients
oto stop
```

---

# 6. Connection Model

The host creates a session:

```text
Session
├── session ID
├── connection code
├── host address
├── UDP audio port
└── control port
```

The host advertises itself using mDNS/Bonjour.

The client:

```text
join code
    ↓
mDNS discovery
    ↓
find matching session
    ↓
connect
```

The IP remains an implementation detail.

---

# 7. Audio Synchronization

Oto must **not** simply play audio as soon as packets arrive.

Instead:

```text
Audio
  ↓
Timestamp
  ↓
Network
  ↓
Jitter buffer
  ↓
Clock correction
  ↓
Scheduled playback
```

All clients target the same playback timestamp.

---

# 8. Bluetooth

Bluetooth latency is considered separately from network synchronization.

Example:

```text
Laptop A → Speaker A = 90ms
Laptop B → Speaker B = 130ms
```

Oto should eventually compensate:

```text
Speaker A → +40ms
Speaker B → +0ms
```

### MVP

Manual latency offset:

```bash
oto latency +40ms
```

### Later

Automatic latency calibration.

---

# 9. Failure Handling

If a client loses Wi-Fi:

```text
Connected
   ↓
Disconnected
   ↓
Reconnecting...
   ↓
Clock resync
   ↓
Buffer refill
   ↓
Playing
```

Packet loss should not stop playback.

Missing packets are detected through sequence numbers.

---

# 10. Success Metrics

For two laptops on a stable local Wi-Fi network:

* Connection established within 5 seconds
* No manual IP configuration
* Continuous audio playback
* Automatic client reconnection
* Synchronization stable over long playback
* No obvious audible echo under normal conditions

The primary technical metric is:

> **Inter-device audio playback timing difference in milliseconds.**

---

# Oto — Technical Requirements Document (TRD)

## 1. Architecture

```text
                         OTO HOST
                    ┌─────────────────┐
                    │ Audio Capture   │
                    └────────┬────────┘
                             │
                             ▼
                       Audio Encoder
                             │
                             ▼
                        Packetizer
                             │
                    ┌────────▼────────┐
                    │  Sync Server    │
                    └────────┬────────┘
                             │
                         Wi-Fi / UDP
                    ┌────────┴────────┐
                    ▼                 ▼
                CLIENT A          CLIENT B
                    │                 │
              Jitter Buffer     Jitter Buffer
                    │                 │
              Sync Scheduler    Sync Scheduler
                    │                 │
              Audio Playback    Audio Playback
                    │                 │
                Bluetooth         Bluetooth
                    │                 │
                   🔊                🔊
```

---

# 2. Technology

### Language

**Rust**

Reasons:

* Low-level audio access
* Strong networking support
* Excellent concurrency
* Single native binary
* Good cross-platform potential
* Good CLI ecosystem

### Initial platform

**macOS**

Later:

```text
macOS
Linux
Windows
```

---

# 3. Dependencies

Potential crates:

```text
tokio
clap
serde
serde_json
uuid
tracing
anyhow
thiserror
mdns-sd
```

Audio implementation:

```text
macOS
   ↓
CoreAudio
```

Create an abstraction:

```rust
trait AudioBackend {
    fn devices();
    fn capture();
    fn playback();
}
```

---

# 4. Project Structure

```text
oto/
├── Cargo.toml
├── README.md
│
├── src/
│   ├── main.rs
│   │
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── host.rs
│   │   ├── join.rs
│   │   └── devices.rs
│   │
│   ├── host/
│   │   ├── mod.rs
│   │   └── session.rs
│   │
│   ├── client/
│   │   ├── mod.rs
│   │   └── session.rs
│   │
│   ├── discovery/
│   │   └── mdns.rs
│   │
│   ├── network/
│   │   ├── protocol.rs
│   │   ├── control.rs
│   │   ├── sender.rs
│   │   └── receiver.rs
│   │
│   ├── sync/
│   │   ├── clock.rs
│   │   ├── scheduler.rs
│   │   └── calibration.rs
│   │
│   ├── audio/
│   │   ├── mod.rs
│   │   ├── capture.rs
│   │   ├── playback.rs
│   │   └── device.rs
│   │
│   └── buffer/
│       └── jitter.rs
│
└── tests/
    ├── protocol.rs
    ├── clock.rs
    └── buffer.rs
```

---

# 5. Network Architecture

Separate control and audio traffic.

### Control

Reliable connection:

```text
TCP
```

Used for:

```text
HELLO
AUTH
CLOCK_SYNC
START
STOP
STATUS
RECONNECT
```

### Audio

```text
UDP
```

Used for low-latency audio packets.

---

# 6. Audio Packet

Initial packet:

```text
┌───────────────┬──────────────┬──────────┬────────────┐
│ Sequence      │ Timestamp    │ Duration │ Payload    │
│ 8 bytes       │ 8 bytes      │ 4 bytes  │ N bytes    │
└───────────────┴──────────────┴──────────┴────────────┘
```

Rust representation:

```rust
struct AudioPacket {
    sequence: u64,
    timestamp: u64,
    duration: u32,
    payload: Vec<u8>,
}
```

Use a binary serialization format rather than JSON.

---

# 7. Audio Format

### MVP

```text
PCM
48 kHz
16-bit
Stereo
```

Approximate bandwidth:

```text
48,000 × 16 × 2
= 1.536 Mbps
```

This is acceptable for a normal local Wi-Fi network.

### Later

Add Opus:

```text
Audio
 ↓
Opus
 ↓
UDP
```

This reduces bandwidth and provides packet-loss resilience.

---

# 8. Clock Synchronization

The host acts as the timing authority.

Use an NTP-style exchange:

```text
HOST                         CLIENT

 T1 ──────────────────────────>

       <──────────────────── T2

 T3 ──────────────────────────>

       <──────────────────── T4
```

Calculate:

```text
RTT =
(T4 - T1) - (T3 - T2)
```

and estimate clock offset:

```text
offset =
((T2 - T1) + (T3 - T4)) / 2
```

Perform multiple measurements and favor low-RTT samples.

Clock synchronization should continue periodically.

---

# 9. Playback Scheduler

Each packet has:

```text
host_timestamp
```

Client calculates:

```text
local_playback_time =
    host_timestamp
    + clock_offset
    + latency_offset
```

Then:

```text
packet
  ↓
jitter buffer
  ↓
scheduler
  ↓
CoreAudio
```

The scheduler must provide sufficiently precise timing rather than depending on packet arrival.

---

# 10. Jitter Buffer

Initial configuration:

```text
Target: 100 ms
Minimum: 50 ms
Maximum: 500 ms
```

The buffer should:

* Reorder packets
* Detect missing packets
* Absorb network jitter
* Feed the playback scheduler continuously

Later it can become adaptive.

---

# 11. Session Discovery

Host publishes:

```text
_syncaudio._udp.local
```

I'd rename this to:

```text
_oto._udp.local
```

Advertisement metadata:

```text
session_id
connection_code
hostname
port
protocol_version
```

Client searches for Oto sessions and matches the supplied code.

---

# 12. Session Authentication

Connection code:

```text
7K4P9
```

should be a human-friendly identifier, **not the cryptographic secret**.

Internally:

```text
Session ID
+
Random authentication token
```

The short code prevents accidental connections while the token handles actual session authentication.

---

# 13. Bluetooth Latency Model

Maintain:

```rust
struct OutputCalibration {
    device_id: String,
    latency_offset_ms: i32,
}
```

For example:

```text
JBL Flip 6
latency: +42ms
```

Eventually:

```bash
oto calibrate
```

could perform an acoustic calibration process.

---

# 14. Reconnection

Client maintains:

```text
CONNECTED
    ↓
CONNECTION_LOST
    ↓
DISCOVER_HOST
    ↓
RECONNECT
    ↓
CLOCK_SYNC
    ↓
BUFFER
    ↓
PLAYING
```

It should not require the user to run `oto join` again.

---

# 15. Development Phases

### Phase 1 — Audio transport

```text
Capture → UDP → Playback
```

Ignore synchronization.

### Phase 2 — Synchronization

Add:

* timestamps
* clock synchronization
* jitter buffer
* scheduled playback

### Phase 3 — Discovery

Add:

* mDNS
* connection codes
* automatic host discovery

### Phase 4 — Audio devices

Add:

* device enumeration
* Bluetooth selection
* output switching

### Phase 5 — Reliability

Add:

* reconnection
* packet-loss handling
* adaptive buffering

### Phase 6 — Calibration

Add:

* manual latency offset
* automatic Bluetooth latency calibration

### Phase 7 — Multi-client

```text
oto host

          ┌── Laptop B
Host ─────┼── Laptop C
          ├── Laptop D
          └── Laptop E
```

---

## 16. MVP Definition

The first milestone should be deliberately small:

```text
Laptop A
  │
  │ capture PCM
  │
  │ UDP
  ▼
Laptop B
  │
  ▼
speaker
```

Then immediately add:

```text
timestamps
+
clock sync
+
jitter buffer
+
scheduled playback
```

Once that works reliably, build the polished:

```bash
oto host
oto join 7K4P9
```

experience around it.


Yes. A `curl` installer is a good fit for **Oto**, especially for a CLI tool.

The target UX can be:

```bash
curl -fsSL https://oto.audio/install.sh | sh
```

Then:

```bash
oto host
```

### Installation design

The installer should:

1. Detect OS and architecture.
2. Download the appropriate prebuilt binary.
3. Verify its checksum/signature.
4. Install to a user-writable location such as `~/.local/bin`.
5. Add that directory to `PATH` if necessary.
6. Print the installed version.

Example:

```text
Installing Oto...

✓ Detected macOS arm64
✓ Downloaded Oto v0.1.0
✓ Verified checksum
✓ Installed /Users/you/.local/bin/oto

Run:

  oto host
```

### GitHub Releases

I'd use GitHub Releases initially:

```text
oto/
├── oto-darwin-arm64
├── oto-darwin-x86_64
├── oto-linux-arm64
├── oto-linux-x86_64
└── checksums.txt
```

And:

```text
install.sh
```

would select the appropriate artifact.

For your M3 Mac, for example:

```text
macOS
 ↓
arm64
 ↓
oto-darwin-arm64
```

### One important change to the TRD

Add an **Installation & Distribution** section:

```text
Installation

Primary:
curl -fsSL https://oto.audio/install.sh | sh

Alternative:
brew install oto

Development:
cargo install oto
```

I'd make **GitHub Releases + curl installer** the MVP distribution mechanism and add Homebrew once the binary/repository naming is settled.

Also, for production, avoid making the install script itself download and execute arbitrary remote shell content beyond the intended release artifact; **checksum/signature verification should be part of the installer design from day one.**


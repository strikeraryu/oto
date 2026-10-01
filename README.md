# Oto

Oto streams a Mac's system audio to other Macs on the same local network. The host and clients play a buffered copy against a shared timeline. Built-in, USB, and paired Bluetooth speakers use normal macOS audio output devices.

Requires **macOS 14.2 or later**. The CLI is Rust; a bundled Swift helper uses Core Audio process taps and hardware playback callbacks. No virtual audio driver is required.

## Build and run

Install Rust and Xcode Command Line Tools, then:

```sh
cargo build --release
./target/release/oto doctor
./target/release/oto host
```

The host prints a five-character code. On another Mac:

```sh
oto join 7K4P9
```

Play audio in any app on the host. Oto captures the system mix, mutes its original direct output while the tap is active, and plays the same delayed stream on the host and clients. Allow system audio recording when macOS prompts; if needed, enable Oto or the launching terminal in **System Settings → Privacy & Security → Screen & System Audio Recording**. Allow incoming network connections on the host. Normal source output is restored when the session ends.

Keep `host` and `join` running in their terminals. Ctrl-C or `oto leave` stops the local session. Closing a host ends its session; clients keep retrying and can find a replacement host with the same code.

## Commands

```sh
oto host                         # System audio; default 200ms buffer
oto host --no-code               # Direct connections without a connection code
oto host --port 9000             # Override the default TCP port, 47670
oto host --source tone           # Quiet 440Hz diagnostic tone, no capture permission
oto host --buffer-ms 300          # More time for Wi-Fi and Bluetooth output buffering
oto join 7K4P9                   # Bonjour discovery, no IP required
oto join --host 192.168.1.14      # No-code host; default port 47670
oto join --host 192.168.1.14:9000 # No-code host with a custom port
oto status                      # Session, devices, packet and clock statistics
oto status --json
oto leave
oto devices                     # Output names and persistent UIDs
oto device "JBL Flip 6"          # Exact name or UID; applies to a running session
oto device default              # Follow macOS output changes automatically
oto latency +40ms               # Delay this output by another 40ms
oto latency                     # Show its saved delay
oto doctor
```

By default, Oto follows the macOS output selection throughout a session. Connect a Bluetooth speaker and select it in macOS Sound settings or Control Center; both hosts and clients switch their local playback automatically. `oto status` shows the new output. Selecting an explicit name or UID with `oto device` pins that output; run `oto device default` to resume following macOS.

Offsets are saved per explicitly selected output UID. The `default` selection has its own saved offset. Positive offsets add delay: if speaker A is 40ms faster than speaker B, apply `+40ms` on Mac A. Negative offsets consume buffering time; Oto requires at least 50ms to remain. The default buffer is 200ms, configurable from 50–500ms.

If Bonjour is blocked by a router, VPN, or firewall:

```sh
oto host --port 47670 --code 7K4P9
oto join 7K4P9 --host 192.168.1.12:47670
```

The host uses TCP port **47670** by default. Override it with `--port`; `--port 0` requests a dynamically allocated control port. Audio uses a separate dynamically allocated UDP port. Both Macs must be reachable on the LAN; client isolation on guest Wi-Fi prevents this. `--no-discovery` disables the advertisement for direct connections.

To connect directly without any code:

```sh
oto host --no-code
oto join --host 192.168.1.14
```

`--no-code` generates no connection code and skips the code check. Anyone who can reach that host on the LAN can join. The client must provide `--host` when omitting its code. Code-required hosts still reject connections with a missing or incorrect code. `--no-code` and `--code` cannot be combined.

## Timing and transport

Audio is stereo PCM, 48kHz, signed 16-bit little-endian. Five-millisecond packets are 1,024 bytes, below a standard LAN MTU. Bandwidth is approximately 1.64Mbps per client including Oto's packet header, plus network overhead. The host supports up to 16 simultaneous clients.

The control channel uses bounded JSON messages over TCP. A 12-sample NTP-style exchange estimates client-minus-host monotonic clock offset; low-RTT measurements are refreshed every two seconds, with gradual corrections. UDP packets contain a protocol version, session ID, per-connection random token, sequence number, presentation timestamp, and PCM. Clients reorder packets briefly, then place audio into the native timestamp ring. Hardware callback timestamps determine which samples to render; rate conversion follows that timeline rather than an accumulating software sleep loop. Missing samples render as silence. Connections resynchronize and refill the buffer after interruption.

Bonjour advertises `_oto._tcp.local.` because discovery leads to the reliable control endpoint. The short code is public discovery metadata and prevents accidental joins. Per-client tokens identify audio packets from that connection. **The MVP uses plaintext TCP/UDP on a trusted LAN; the code and token do not provide encryption or protection against a malicious LAN participant.**

Bluetooth speakers add device-dependent acoustic delay. Core Audio timing alone does not measure that delay; use manual offsets. This implementation has no automatic acoustic calibration and makes no measured inter-speaker synchronization claim yet.

## Installation and releases

Until a release repository is configured, build locally or run `cargo install --path .`. The release workflow builds archives for Apple Silicon and Intel, each containing one `oto` binary with its audio helper embedded. Release assets include SHA-256 checksums.

Once you publish a tagged release in your GitHub repository:

```sh
curl -fsSL https://raw.githubusercontent.com/OWNER/REPO/main/install.sh -o /tmp/oto-install.sh
OTO_REPOSITORY=OWNER/REPO sh /tmp/oto-install.sh
```

The installer verifies the archive against the release checksum list and installs into `~/.local/bin`. It prints the PATH setup command if needed. `OTO_VERSION=v0.1.0` selects a version; `OTO_INSTALL_DIR` selects an installation directory. A production distribution still needs a stable repository/domain and Developer ID signing/notarization. Development builds ad-hoc sign the audio helper and embed the audio-capture usage description.

## Development and checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# Optional silent native output lifecycle check, on a Mac with an output device:
cargo test --test native -- --ignored
```

Tests cover clock offset math, malformed and unauthorized packets, control message limits, reordering/loss, live status/latency controls, multiple loopback clients, host restart and reconnection, and Bonjour discovery. Transport integration tests use a diagnostic source and a headless renderer; they do not grant capture permission or play sound. `OTO_CONFIG_DIR` isolates session state for development and tests; normal state lives in `~/Library/Application Support/Oto`.

For a physical check, run the diagnostic tone on two Macs, calibrate the speaker offsets, and record both outputs with a shared microphone or recorder. Compare transient arrival times over an extended run and after a Wi-Fi interruption. Then repeat with system capture. Audible alignment and long-run Bluetooth drift require this hardware check.

The product specification is in [DESIGN.MD](DESIGN.MD). Automatic acoustic calibration, Opus, adaptive network buffering, Internet sessions, mobile clients, and Linux/Windows audio backends are future work.

# Oto

Oto streams a Mac's system audio to other Macs on the same local network. The host and clients play a buffered copy against a shared timeline. Built-in, USB, and paired Bluetooth speakers use normal macOS audio output devices.

Requires **macOS 14.2 or later**. The CLI is Rust; a bundled Swift helper uses Core Audio process taps and hardware playback callbacks. No virtual audio driver is required.

## Build and run

Install Rust and Xcode Command Line Tools, then:

```sh
cargo build --release
./target/release/oto doctor
./target/release/oto
```

## Interactive terminal interface

Run `oto` (or `oto tui`) to open the TUI. The header shows this Mac's local IP; F4 lists all network addresses, and `?` opens keyboard help. Requires an interactive terminal at least 64 columns by 26 rows.

- **Host:** choose with-code or no-code connections, set a port (default `47670`) and buffer (default `200` ms), then select **Start hosting**. The session view shows its code, actual port, addresses, and connected clients.
- **Join:** choose with-code or no-code connections. With a code, leave the IP blank for automatic discovery or enter an IP and port for a direct connection. No-code connections require a host IP. The session view shows the code when used and the host address once connected.
- **Navigate:** Tab / Shift-Tab or Up / Down moves between fields; Left / Right selects the Host / Join tab. Enter selects an action; Space toggles the connection mode. F1 opens Host, F2 opens Join, and Ctrl-U clears a field. Paste IPs and codes directly into their fields.
- **During playback:** `+` (or `=`) / `-` increases or decreases **this speaker's reported delay** by **10 ms**; `]` / `[` changes it by **1 ms**. `0` resets its report. If your Bluetooth speaker sounds late, increase the value on the Mac connected to that speaker. The host automatically adds the required delay to faster outputs throughout the session. Reports range from 0–500 ms and remain separate from clock synchronization.
- **Output and session controls:** `d` opens the local output picker, `s` stops hosting or leaves the joined session, and `q` opens quit controls. Tab and Enter also operate the visible action buttons.
- **Logs:** Page Up / Page Down scrolls the most recent 500 log lines. End resumes following new messages during a session. Logs include startup errors, connections, reconnect attempts, output changes, and latency adjustments.

The TUI manages sessions it starts and stops them on exit. If a CLI session is already running, it shows that session's status and provides local controls; quit controls offer `d` to close the TUI while leaving that existing session running. Existing session logs from before attachment are not available, but subsequent observed status changes appear in the log panel.

For Bluetooth alignment, report the delay on the Mac connected to the late speaker. For example:

| Output | Reported speaker delay | Automatic added delay |
|---|---:|---:|
| A: Bluetooth | 150 ms | 0 ms |
| B: Built-in | 0 ms | 150 ms |
| C: USB | 30 ms | 120 ms |

Each device reports once; the host recalculates compensation when reports change or participants join/leave. The session view shows **This speaker's delay**, **Auto added**, and the slowest speaker's **Target**. Reports are saved by actual output UID, including while following the macOS default, and restored and sent to the host when switching outputs. Other clients' compensation statistics refresh with their next clock exchange (within roughly two seconds); their audio timestamps are updated by the host on subsequent packets.

The previous TUI's values meant extra local playback delay. Those values are preserved in the legacy `offsets` settings map and are no longer applied. New speaker-delay estimates start at zero; enter the estimate on each late speaker. Update the binary on **every Mac** and restart sessions: this coordination uses protocol version 2 and cannot mix with version 1 hosts or clients. Changing reports during playback can briefly skip or pause sound while queued audio settles onto the new timing.

## Command-line use

You can also run `oto host` directly. The host prints its local IP address, TCP port, a five-character code, and a copyable join command. On another Mac:

```sh
oto join 7K4P9
```

Play audio in any app on the host. Oto captures the system mix, mutes its original direct output while the tap is active, and plays the same delayed stream on the host and clients. Allow system audio recording when macOS prompts; if needed, enable Oto or the launching terminal in **System Settings → Privacy & Security → Screen & System Audio Recording**. Allow incoming network connections on the host. Normal source output is restored when the session ends.

Keep `host` and `join` running in their terminals. Ctrl-C or `oto leave` stops the local session. Closing a host ends its session; clients keep retrying and can find a replacement host with the same code.

## Commands

```sh
oto                             # Interactive terminal interface
oto tui                         # Explicitly open the TUI
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
oto latency 150ms               # Report this speaker's estimated delay
oto speaker-delay 150ms         # Alias for oto latency
oto latency                     # Show this speaker's saved estimate
oto doctor
```

By default, Oto follows the macOS output selection throughout a session. Connect a Bluetooth speaker and select it in macOS Sound settings or Control Center; both hosts and clients switch their local playback automatically. `oto status` shows the new output. Selecting an explicit name or UID with `oto device` pins that output; run `oto device default` to resume following macOS.

Speaker-delay estimates are saved per actual output UID. `oto latency 150ms` sets an absolute estimate; the TUI shortcuts increment or decrement it. Estimates are nonnegative, from 0–500 ms. The default network buffer is 200 ms, configurable from 50–500 ms, and remains available to every participant independently of speaker compensation.

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

The control channel uses bounded JSON messages over TCP. Clients report their speaker delay during the handshake and clock exchanges; changing the value triggers an immediate exchange. A 12-sample NTP-style exchange estimates client-minus-host monotonic clock offset; low-RTT measurements are refreshed every two seconds, with gradual corrections. The host finds the slowest reported output, including its own, and schedules each device at `capture time + network buffer + slowest delay − device delay`. UDP packets contain a protocol version, session ID, per-connection random token, sequence number, a hardware playback timestamp compensated for that receiver, and PCM. Clients add only their clock offset, reorder packets briefly, then place audio into the native timestamp ring. Hardware callback timestamps determine which samples to render; rate conversion follows that timeline rather than an accumulating software sleep loop. Missing samples render as silence. Connections resynchronize, re-report speaker delay, and refill the buffer after interruption.

Bonjour advertises `_oto._tcp.local.` because discovery leads to the reliable control endpoint. The short code is public discovery metadata and prevents accidental joins. Per-client tokens identify audio packets from that connection. **The MVP uses plaintext TCP/UDP on a trusted LAN; the code and token do not provide encryption or protection against a malicious LAN participant.**

Bluetooth speakers add device-dependent acoustic delay. Oto coordinates compensation automatically from your reported estimates; it does not automatically measure or acoustically calibrate those estimates. Accurate audible alignment requires tuning those values on the actual speakers.

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

For a physical check, run the diagnostic tone on two Macs, tune the reported speaker delays, and record both outputs with a shared microphone or recorder. Compare transient arrival times over an extended run and after a Wi-Fi interruption. Then repeat with system capture. Audible alignment and long-run Bluetooth drift require this hardware check.

The product specification is in [DESIGN.MD](DESIGN.MD). Automatic acoustic calibration, Opus, adaptive network buffering, Internet sessions, mobile clients, and Linux/Windows audio backends are future work.

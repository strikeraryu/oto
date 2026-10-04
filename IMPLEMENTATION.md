# macOS implementation decisions

This implementation follows the MVP in `DESIGN.MD`, with the following concrete choices:

- macOS 14.2+ is required for native Core Audio process taps. A Swift engine is compiled at build time, ad-hoc signed, embedded in the Rust binary, and extracted to a content-addressed private directory. Its Info.plist contains the system-audio capture usage description.
- The host captures system audio using a private global stereo tap, excludes its own audio process, and uses `mutedWhenTapped` so the original source does not also play immediately. Both host and clients render Oto's buffered PCM.
- PCM packets contain 240 frames (5ms). Larger packets would exceed common MTUs at the specified 48kHz/16-bit/stereo format. The binary header is versioned and validates the session and per-client token.
- `_oto._tcp.local.` advertises the control endpoint. TCP port 47670 is the default; `--port` overrides it and `--port 0` requests an ephemeral port. The host logs its local IP addresses and copyable join commands; status includes these addresses. The UDP endpoint is negotiated during the TCP handshake. `host --no-code` does not generate or require a code; `join --host IP[:PORT]` connects directly without one. IPv4 LAN hosting is the default; direct addresses are available if multicast discovery is blocked.
- Clock exchanges use `t1/t4` on the client and `t2/t3` on the host. The estimated offset is **client minus host**, so the client adds it to host presentation timestamps. All local timestamps use Mach absolute time, never wall clock.
- Default shared buffering is 200ms (50–500ms configurable), allowing time for both network jitter and hardware buffering. A bounded reorder queue feeds a native absolute timestamp ring. Native render callbacks map their output host timestamps onto 48kHz sample positions, rate converting to the selected hardware's sample rate. Dropped samples become silence.
- Speaker-delay estimates are saved per actual output UID and reported to the host. The host coordinates each output against the maximum report using `capture + network buffer + maximum report − device report`; each device retains the full configured network buffer. In default mode, both host and client observe Core Audio's default-output property and switch playback when macOS selects another device. Explicit output selections remain pinned. Native output events restore the report for that speaker and trigger a fresh client report.
- `host` and `join` stay in the foreground. A private Unix socket supplies status, leave and live settings commands. SIGINT/SIGTERM and pipe EOF stop the helper and destroy its private tap/aggregate objects.
- The short code is advertised and is not a secret. Per-connection random tokens prevent accidental cross-session audio. Transport is plaintext and intended for a trusted LAN. Cryptographic peer authentication/encryption needs a separate pairing design.
- Connection loss triggers discovery, a new handshake/token, fresh clock synchronization, and buffer refill. The first release supports up to 16 clients and fixed buffering. Acoustic calibration, adaptive buffering and Opus are deferred as described in the product scope.
- Release automation produces ad-hoc signed arm64 and x86_64 archives with SHA-256 checksums and attaches `install.sh` to the release. The one-command installer defaults to `strikeraryu/oto`, resolves latest to an immutable tag, validates the checksum and runnable binary, and atomically installs into `~/.local/bin` without sudo. It configures zsh/bash login PATH with an opt-out and supports pinned versions and custom installation directories.

## Quick installation (2026-10-04)

- The README and release notes expose the release-hosted one-command installer. Binary installation does not require Rust or Xcode; `oto` opens the TUI after the shell PATH is available.
- The installer performs all mutations inside a final function invocation so a truncated script fails to parse before installation starts. It validates input, restricts downloads/redirects to HTTPS, uses bounded curl retries/timeouts, and cleans up temporary/staged files on failure or signals.
- Existing installations are replaced only after checksum verification and a successful `--version` invocation of the downloaded binary. PATH lines escape shell metacharacters and are not duplicated; bash's existing login profile takes precedence. `--no-modify-path` supports managed shell environments.
- The release workflow signs/verifies each main binary, builds both architecture archives, and publishes the installer and checksum list from the tagged source alongside the binaries.

## Terminal interface (2026-10-04)

- `oto` and `oto tui` open a Ratatui/Crossterm interface with Host/Join forms, a local-IP header, live session statistics, output selection, and a bounded log panel. Existing CLI commands remain available.
- The interface starts the existing executable as a managed child with a separate process group, captures its output, and uses the Unix control socket for status and commands. Audio and timing remain in the session process. Existing CLI sessions can be controlled without spawning another session.
- Status polling and serialized control requests run in an asynchronous worker; rendering and keyboard input remain responsive during connection and control errors. Tab/arrow navigation, bracketed paste, contextual keyboard help, and quit confirmation cover the main flows.
- Speaker-delay increments are applied atomically in the session runtime. Requests carry the observed output UID so a queued change is rejected if the output changed. Reports remain separate from clock synchronization; the host applies compensation in each receiver's packet timestamps.
- The audio helper's active output UID is exposed in status. Settings persist `speaker_delays` independently of the user's default/pinned output preference. Previous additional local `offsets` are retained but are not reused or applied because they represent a different measurement.
- Terminal state is restored on normal exit, errors, and panic. Managed sessions use IPC shutdown with signal/kill fallbacks during startup or unresponsive shutdown; attached sessions can be left running by choosing detach in the quit dialog.
- Host coordination automatically compensates for reported speaker delays, including its own output. Acoustic measurement and automatic estimation of Bluetooth delay remain future work.
- Build checks: `cargo fmt --check`, `cargo clippy --locked --offline --all-targets -- -D warnings`, `cargo build --locked --offline --release`, and `git diff --check` passed. Tests and interactive/physical audio checks were not run for this change.

## Reported speaker-delay coordination (2026-10-04)

- Protocol version 2 adds bounded 0–500 ms reports to `Hello`/`Sync` and returns the slowest report in `Welcome`/`Synced`. Every participating Mac must run the updated binary. Existing handshake fixtures use the current version.
- The host snapshots all current reports once per captured packet, computes a common acoustic target, and gives the host and each client their own hardware deadline. Clients apply only their clock correction, preventing double application of a report.
- A watch notification sends changed reports without waiting for the periodic clock exchange. Reports are restored after output changes and sent again after reconnecting. Peer removal drops its report from the next playback plan.
- Status exposes reported delay, last acknowledged report, slowest output delay, and calculated compensation. TUI wording and the `oto latency` / `oto speaker-delay` commands use speaker-delay semantics. Reports below zero or above 500 ms are rejected.
- The maximum hardware lead is 1 second (500 ms buffer plus 500 ms compensation), within the native 2-second ring. The jitter queue allows 1.5 seconds of future timestamps for clock/capture margin.
- Changing reports can cause a short skip or silence while old queued timestamps settle. Physical alignment depends on the accuracy of the reports and has not been measured here.
- Formatting, Clippy with all targets, the release build, and diff checks passed for this update. Tests and physical audio checks were not run.

## Validation boundary

Automated transport checks use a tone source and a headless renderer. They verify network and control behavior, not physical Core Audio output timing. Native device enumeration and compilation can be checked on one Mac; system recording permission, simultaneous speaker alignment, Bluetooth delay, device disconnects and long playback require a physical check on at least two Macs. Developer ID signing/notarization requires release credentials.

## Checks completed — 2026-10-01

- `cargo fmt --check` passed.
- `cargo clippy --locked --all-targets -- -D warnings` passed.
- `cargo test --locked` passed six unit tests and two network integration tests. The separate hardware-dependent test is ignored in the default suite.
- `cargo test --locked --test native -- --ignored` passed the silent native output startup/EOF cleanup check on this Mac.
- `oto doctor` found MacBook Pro Speakers and ZoomAudioDevice on macOS 26.5.2.
- The Swift helper compiled for both arm64 and x86_64 with a macOS 14.2 deployment target.
- `cargo build --locked --release` produced `target/release/oto`, a 3MB arm64 executable.
- `sh -n install.sh` passed. Release publication and remote installer downloads require a configured GitHub repository and have not been exercised.
- System audio capture and physical alignment across two Macs have not been exercised; the recording permission must be granted when first starting a system-source host.

## Updates verified — 2026-10-03

- Formatting and Clippy checks passed. Nine unit tests and three network integration tests passed, covering default/override ports, optional codes, code-required rejection, IP logging and copyable join commands.
- The separate native test passed a silent switch to another connected output and back to the system default, followed by EOF cleanup. The helper also compiled for Intel Macs.
- The arm64 release binary was rebuilt. Automatic handoff to a real Bluetooth speaker and physical speaker alignment still require a hardware check.

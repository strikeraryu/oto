# macOS implementation decisions

This implementation follows the MVP in `DESIGN.MD`, with the following concrete choices:

- macOS 14.2+ is required for native Core Audio process taps. A Swift engine is compiled at build time, ad-hoc signed, embedded in the Rust binary, and extracted to a content-addressed private directory. Its Info.plist contains the system-audio capture usage description.
- The host captures system audio using a private global stereo tap, excludes its own audio process, and uses `mutedWhenTapped` so the original source does not also play immediately. Both host and clients render Oto's buffered PCM.
- PCM packets contain 240 frames (5ms). Larger packets would exceed common MTUs at the specified 48kHz/16-bit/stereo format. The binary header is versioned and validates the session and per-client token.
- `_oto._tcp.local.` advertises the control endpoint. The UDP endpoint is negotiated during the TCP handshake. IPv4 LAN hosting is the default; direct addresses are available if multicast discovery is blocked.
- Clock exchanges use `t1/t4` on the client and `t2/t3` on the host. The estimated offset is **client minus host**, so the client adds it to host presentation timestamps. All local timestamps use Mach absolute time, never wall clock.
- Default shared buffering is 200ms (50–500ms configurable), allowing time for both network jitter and hardware buffering. A bounded reorder queue feeds a native absolute timestamp ring. Native render callbacks map their output host timestamps onto 48kHz sample positions, rate converting to the selected hardware's sample rate. Dropped samples become silence.
- Manual output offsets are additional software delay, saved by selected device UID or by the `default` selection. At least 50ms of shared buffering must remain after negative offsets. Device changes and offsets can be applied through local IPC while a session runs.
- `host` and `join` stay in the foreground. A private Unix socket supplies status, leave and live settings commands. SIGINT/SIGTERM and pipe EOF stop the helper and destroy its private tap/aggregate objects.
- The short code is advertised and is not a secret. Per-connection random tokens prevent accidental cross-session audio. Transport is plaintext and intended for a trusted LAN. Cryptographic peer authentication/encryption needs a separate pairing design.
- Connection loss triggers discovery, a new handshake/token, fresh clock synchronization, and buffer refill. The first release supports up to 16 clients and fixed buffering. Acoustic calibration, adaptive buffering and Opus are deferred as described in the product scope.
- Release automation produces arm64 and x86_64 archives with SHA-256 checksums. The installer requires a real GitHub release repository; no placeholder domain is presented as an operational download endpoint.

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

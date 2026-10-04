Oto streams synchronized system audio between Macs on the same local network.
Requires macOS 14.2 or later. Binaries for Apple Silicon and Intel include the audio helper; Rust and Xcode are not needed to install.

### Quick install

```sh
curl -fsSL https://github.com/strikeraryu/oto/releases/latest/download/install.sh | sh
```

Open a new terminal or run the PATH command printed by the installer, then run `oto` for the Host / Join interface. Rerun the installer to upgrade.

### Audio setup

Allow system audio recording when macOS prompts while hosting. If a Bluetooth speaker sounds late, increase **This speaker's delay** on the Mac connected to that speaker; Oto coordinates compensation for other outputs.

Update Oto on every participating Mac and restart running sessions after upgrading. This release uses protocol version 2. Speaker delay is a saved estimate; automatic acoustic measurement is not implemented.

Release binaries and their embedded audio helper are ad-hoc signed. Developer ID signing and notarization are not configured.

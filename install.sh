#!/bin/sh
set -eu

fail() { printf 'oto installer: %s\n' "$*" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || fail 'macOS 14.2 or later is required'
case "$(uname -m)" in
    arm64) oto_arch=arm64 ;;
    x86_64) oto_arch=x86_64 ;;
    *) fail 'unsupported architecture' ;;
esac
oto_os_version=$(sw_vers -productVersion)
oto_major=${oto_os_version%%.*}
oto_minor=${oto_os_version#*.}
oto_minor=${oto_minor%%.*}
[ "$oto_major" -gt 14 ] || { [ "$oto_major" -eq 14 ] && [ "$oto_minor" -ge 2 ]; } || fail 'macOS 14.2 or later is required'
oto_repo=${OTO_REPOSITORY:-}
case "$oto_repo" in
    ''|*[!A-Za-z0-9_./-]*|*..*) fail 'set OTO_REPOSITORY to the GitHub OWNER/REPO containing Oto releases' ;;
esac
case "$oto_repo" in
    */*/*|/*|*/) fail 'OTO_REPOSITORY must be OWNER/REPO' ;;
    */*) ;;
    *) fail 'OTO_REPOSITORY must be OWNER/REPO' ;;
esac
oto_version=${OTO_VERSION:-latest}
case "$oto_version" in *[!A-Za-z0-9._-]*|'') fail 'invalid OTO_VERSION' ;; esac
if [ "$oto_version" = latest ]; then
    oto_base="https://github.com/$oto_repo/releases/latest/download"
else
    oto_base="https://github.com/$oto_repo/releases/download/$oto_version"
fi
oto_archive="oto-darwin-$oto_arch.tar.gz"
oto_temp=$(mktemp -d "${TMPDIR:-/tmp}/oto-install.XXXXXXXX")
trap 'rm -rf "$oto_temp"' EXIT HUP INT TERM
printf 'Downloading Oto for macOS %s…\n' "$oto_arch"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$oto_base/$oto_archive" -o "$oto_temp/$oto_archive"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 "$oto_base/checksums.txt" -o "$oto_temp/checksums.txt"
oto_checksum=$(awk -v name="$oto_archive" '$2 == name {print $1}' "$oto_temp/checksums.txt")
[ "${#oto_checksum}" -eq 64 ] || fail 'missing or invalid checksum'
case "$oto_checksum" in *[!A-Fa-f0-9]*) fail 'invalid checksum' ;; esac
oto_actual=$(shasum -a 256 "$oto_temp/$oto_archive")
oto_actual=${oto_actual%% *}
[ "$oto_checksum" = "$oto_actual" ] || fail 'checksum verification failed'
# Extract the one known file; ignore any other archive entries.
tar -xzf "$oto_temp/$oto_archive" -C "$oto_temp" oto
[ -f "$oto_temp/oto" ] && [ ! -L "$oto_temp/oto" ] || fail 'release binary is missing'
oto_dir=${OTO_INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$oto_dir"
oto_staged=$(mktemp "$oto_dir/.oto-install.XXXXXXXX")
cp "$oto_temp/oto" "$oto_staged"
chmod 755 "$oto_staged"
mv -f "$oto_staged" "$oto_dir/oto"
printf 'Installed %s\n' "$oto_dir/oto"
"$oto_dir/oto" --version
case ":$PATH:" in
    *":$oto_dir:"*) ;;
    *) printf '\nAdd to your shell profile:\n  export PATH="%s:$PATH"\n' "$oto_dir" ;;
esac
printf '\nRun: oto doctor\nThen: oto host\n'

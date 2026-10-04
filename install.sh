#!/bin/sh
# Keep work inside functions so an incomplete curl download cannot start an install.

oto_fail() { printf 'oto installer: %s\n' "$*" >&2; exit 1; }

oto_help() {
    cat <<'USAGE'
Install Oto for macOS 14.2+ (Apple Silicon or Intel), without sudo.

Usage: sh install.sh [options]
  --version TAG       Install a release such as v0.1.0 (default: latest)
  --install-dir PATH  Absolute installation directory (default: ~/.local/bin)
  --no-modify-path    Leave shell profiles untouched
  --help              Show this help

Environment: OTO_VERSION, OTO_INSTALL_DIR, OTO_NO_MODIFY_PATH=1
             OTO_REPOSITORY (default: strikeraryu/oto)
Rerun the installer to upgrade. Restart running sessions after upgrading.
USAGE
}

oto_cleanup() {
    [ -z "$oto_staged" ] || rm -f "$oto_staged"
    [ -z "$oto_temp" ] || rm -rf "$oto_temp"
}

oto_download() {
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --tlsv1.2 --retry 2 --connect-timeout 15 --max-time 180 "$1" -o "$2" ||
        oto_fail "download failed; check your connection and the release assets at https://github.com/$oto_repo/releases"
}

oto_configure_path() {
    case ":$PATH:" in *":$oto_dir:"*) return 0 ;; esac
    # Escape literal path characters before writing a shell command to a profile.
    oto_escaped_dir=$(printf '%s' "$oto_dir" | sed 's/[\\"$`]/\\&/g')
    oto_path_line="export PATH=\"$oto_escaped_dir:\$PATH\""
    printf '\nFor this terminal, run:\n  %s\n' "$oto_path_line"
    [ "$oto_no_modify_path" = 0 ] || return 0
    case "${SHELL:-/bin/zsh}" in
        */zsh) oto_profile="${ZDOTDIR:-$HOME}/.zprofile" ;;
        */bash)
            if [ -f "$HOME/.bash_profile" ]; then oto_profile="$HOME/.bash_profile"
            elif [ -f "$HOME/.bash_login" ]; then oto_profile="$HOME/.bash_login"
            elif [ -f "$HOME/.profile" ]; then oto_profile="$HOME/.profile"
            else oto_profile="$HOME/.bash_profile"
            fi ;;
        *) printf 'Add %s to your shell PATH to use the oto command.\n' "$oto_dir"; return ;;
    esac
    if [ -f "$oto_profile" ] && grep -Fqx "$oto_path_line" "$oto_profile"; then
        printf 'PATH is already configured in %s; reopen your terminal.\n' "$oto_profile"
        return
    fi
    if printf '\n# Oto\n%s\n' "$oto_path_line" >> "$oto_profile"; then
        printf 'Added Oto to PATH in %s. New login terminals will find oto.\n' "$oto_profile"
    else
        printf 'Could not update %s; use the PATH command above.\n' "$oto_profile" >&2
    fi
}

oto_main() {
    set -eu
    oto_version=${OTO_VERSION:-latest}
    oto_dir=${OTO_INSTALL_DIR:-"${HOME:?HOME is unset}/.local/bin"}
    oto_no_modify_path=${OTO_NO_MODIFY_PATH:-0}
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --help|-h) oto_help; return ;;
            --version|--install-dir)
                [ "$#" -ge 2 ] || oto_fail "$1 requires a value"
                case "$1" in --version) oto_version=$2 ;; --install-dir) oto_dir=$2 ;; esac
                shift 2 ;;
            --no-modify-path) oto_no_modify_path=1; shift ;;
            *) oto_fail "unknown option: $1 (use --help)" ;;
        esac
    done
    case "$oto_no_modify_path" in 0|1) ;; *) oto_fail 'OTO_NO_MODIFY_PATH must be 0 or 1' ;; esac
    case "$oto_version" in *[!A-Za-z0-9._+-]*|'') oto_fail 'invalid release version' ;; esac
    case "$oto_dir" in /*) ;; *) oto_fail 'installation directory must be an absolute path' ;; esac
    oto_cr=$(printf '\r')
    case "$oto_dir" in *:*|*"$oto_cr"*|*'
'*) oto_fail 'installation directory cannot contain a colon or newline' ;; esac
    [ ! -d "$oto_dir/oto" ] || oto_fail 'the installation target is a directory; choose another --install-dir'
    oto_repo=${OTO_REPOSITORY:-strikeraryu/oto}
    case "$oto_repo" in ''|*[!A-Za-z0-9_./-]*|*..*) oto_fail 'invalid OTO_REPOSITORY' ;; esac
    case "$oto_repo" in */*/*|/*|*/) oto_fail 'OTO_REPOSITORY must be OWNER/REPO' ;; */*) ;; *) oto_fail 'OTO_REPOSITORY must be OWNER/REPO' ;; esac
    [ "$(uname -s)" = Darwin ] || oto_fail 'macOS 14.2 or later is required'
    case "$(uname -m)" in arm64) oto_arch=arm64 ;; x86_64) oto_arch=x86_64 ;; *) oto_fail 'unsupported architecture' ;; esac
    oto_os_version=$(sw_vers -productVersion)
    oto_major=${oto_os_version%%.*}
    case "$oto_os_version" in *.*) oto_minor=${oto_os_version#*.}; oto_minor=${oto_minor%%.*} ;; *) oto_minor=0 ;; esac
    case "$oto_major:$oto_minor" in *[!0-9:]*) oto_fail 'could not read the macOS version' ;; esac
    [ "$oto_major" -gt 14 ] || { [ "$oto_major" -eq 14 ] && [ "$oto_minor" -ge 2 ]; } || oto_fail 'macOS 14.2 or later is required'
    for oto_tool in curl tar shasum awk sed grep; do
        command -v "$oto_tool" >/dev/null 2>&1 || oto_fail "required macOS utility is missing: $oto_tool"
    done
    if [ "$oto_version" = latest ]; then
        printf 'Finding the latest Oto release...\n'
        oto_release_url=$(curl --fail --silent --show-error --location --head --output /dev/null \
            --write-out '%{url_effective}' --proto '=https' --proto-redir '=https' --tlsv1.2 \
            --retry 2 --connect-timeout 15 --max-time 60 "https://github.com/$oto_repo/releases/latest") ||
            oto_fail "no downloadable release found; see https://github.com/$oto_repo/releases"
        oto_tag_prefix="https://github.com/$oto_repo/releases/tag/"
        case "$oto_release_url" in "$oto_tag_prefix"*) oto_version=${oto_release_url#"$oto_tag_prefix"} ;; *) oto_fail 'no published release is available yet' ;; esac
        case "$oto_version" in *[!A-Za-z0-9._+-]*|''|latest) oto_fail 'invalid release tag returned by GitHub' ;; esac
    fi
    # Resolve latest once; both downloads must come from the same release tag.
    oto_base="https://github.com/$oto_repo/releases/download/$oto_version"
    oto_archive="oto-darwin-$oto_arch.tar.gz"
    oto_temp=$(mktemp -d "${TMPDIR:-/tmp}/oto-install.XXXXXXXX")
    oto_staged=''
    trap oto_cleanup 0
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    printf 'Downloading Oto %s for macOS %s...\n' "$oto_version" "$oto_arch"
    oto_download "$oto_base/$oto_archive" "$oto_temp/$oto_archive"
    oto_download "$oto_base/checksums.txt" "$oto_temp/checksums.txt"
    oto_checksum=$(awk -v name="$oto_archive" '$2 == name {print $1}' "$oto_temp/checksums.txt")
    [ "${#oto_checksum}" -eq 64 ] || oto_fail 'missing or invalid checksum'
    case "$oto_checksum" in *[!A-Fa-f0-9]*) oto_fail 'invalid checksum' ;; esac
    oto_actual=$(shasum -a 256 "$oto_temp/$oto_archive")
    oto_actual=${oto_actual%% *}
    [ "$oto_checksum" = "$oto_actual" ] || oto_fail 'checksum verification failed; the installed binary was not changed'
    printf 'Verified SHA-256 checksum.\n'
    tar -xzf "$oto_temp/$oto_archive" -C "$oto_temp" oto
    [ -f "$oto_temp/oto" ] && [ ! -L "$oto_temp/oto" ] || oto_fail 'release binary is missing'
    chmod 755 "$oto_temp/oto"
    oto_installed_version=$("$oto_temp/oto" --version) || oto_fail 'downloaded binary cannot run on this Mac'
    mkdir -p "$oto_dir"
    oto_staged=$(mktemp "$oto_dir/.oto-install.XXXXXXXX")
    cp "$oto_temp/oto" "$oto_staged"
    chmod 755 "$oto_staged"
    mv -f "$oto_staged" "$oto_dir/oto"
    oto_staged=''
    printf 'Installed %s at %s/oto\n' "$oto_installed_version" "$oto_dir"
    oto_configure_path
    printf '\nRun oto to open the Host / Join interface.\nRun oto doctor to check audio support.\nRestart any running Oto sessions after an upgrade.\n'
}

oto_main "$@"

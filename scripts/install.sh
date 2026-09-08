#!/bin/sh
#
# AutoNet installer, for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/phravins/AUTONET/main/scripts/install.sh | sh
#
# Windows has its own: scripts/install.ps1.
#
# POSIX sh on purpose: under `curl ... | sh` the shebang is never read, so the
# interpreter is whatever the user piped to. No pipefail, no arrays, no `[[`.
#
# It never calls sudo, never extracts an archive it has not checksummed, and
# never overwrites an existing autonet without asking.
#
# Environment:
#
#   AUTONET_VERSION       install this version instead of the latest release
#   AUTONET_INSTALL_DIR   install here instead of the default search
#   AUTONET_FORCE=1       overwrite an existing install without prompting
#
set -eu

REPO="phravins/AUTONET"
RELEASES="https://github.com/${REPO}/releases"

say()  { printf '%s\n' "$*"; }
info() { printf '  %s\n' "$*"; }
err()  { printf 'error: %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

need() {
    have "$1" || err "this installer needs $1, which is not on your PATH."
}

# --- what am I running on? ---------------------------------------------------
#
# These four triples are exactly the four Unix targets release.yml builds.
# Anything else is refused by name rather than guessed at, because the
# alternative is downloading a 404 page and trying to execute it.
detect_target() {
    kernel=$(uname -s)
    machine=$(uname -m)

    case "$kernel" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *) err "unsupported operating system: $kernel (AutoNet ships Linux and macOS builds; on Windows use scripts/install.ps1)" ;;
    esac

    case "$machine" in
        x86_64 | amd64)  arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) err "unsupported architecture: $machine (AutoNet ships x86_64 and aarch64 builds)" ;;
    esac

    # On musl the glibc binary downloads, passes its checksum and then dies with
    # a confusing "not found" from the loader. Caught here, where the message
    # can say why.
    if [ "$kernel" = Linux ] && [ ! -e /lib/ld-linux-x86-64.so.2 ] \
       && [ ! -e /lib/ld-linux-aarch64.so.1 ] && [ ! -e /lib64/ld-linux-x86-64.so.2 ]; then
        if ls /lib/ld-musl-* >/dev/null 2>&1; then
            err "this looks like a musl system (Alpine or similar). AutoNet's Linux
       binaries are linked against glibc and will not run here. Build from
       source instead: cargo install autonet"
        fi
    fi

    printf '%s-%s' "$arch" "$os"
}

# --- downloading -------------------------------------------------------------
if have curl; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif have wget; then
    fetch() { wget -q "$1" -O "$2"; }
else
    err "this installer needs curl or wget, and found neither."
fi

# --- checksums ---------------------------------------------------------------
#
# macOS has no sha256sum and Linux has no shasum, so both spellings are needed.
# With neither, the install is abandoned rather than silently skipping the check
# this whole step exists to perform.
if have sha256sum; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif have shasum; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    err "found neither sha256sum nor shasum, so the download cannot be verified.
       Install one of them, or download from ${RELEASES} and check it by hand."
fi

main() {
    need uname
    need mktemp
    need tar

    target=$(detect_target)

    # SHA256SUMS does double duty: it is the integrity check, and — because
    # release.yml puts the version in every filename — it is also how the
    # version is discovered. GitHub resolves /latest/download/ server-side, so
    # this needs no API call, cannot be rate-limited, and leaves no second
    # source of truth to disagree with the first.
    if [ -n "${AUTONET_VERSION:-}" ]; then
        want=${AUTONET_VERSION#v}
        sums_url="${RELEASES}/download/v${want}/SHA256SUMS"
    else
        want=""
        sums_url="${RELEASES}/latest/download/SHA256SUMS"
    fi

    tmp=$(mktemp -d) || err "could not create a temporary directory."
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "AutoNet installer"
    info "platform   ${target}"

    fetch "$sums_url" "$tmp/SHA256SUMS" \
        || err "could not download ${sums_url}
       If this is a fresh repository there may be no release yet; check
       ${RELEASES}"

    # The line for this platform, which settles the version too.
    line=$(grep -E "  autonet-.*-${target}\.(tar\.gz|zip)$" "$tmp/SHA256SUMS" | head -n 1) \
        || line=""
    [ -n "$line" ] || err "this release has no build for ${target}.
       It contains:
$(sed 's/^[0-9a-f]*  /         /' "$tmp/SHA256SUMS")"

    expected=${line%% *}
    archive=${line##* }
    version=${archive#autonet-}
    version=${version%-${target}.tar.gz}
    info "version    ${version}"

    fetch "${RELEASES}/download/v${version}/${archive}" "$tmp/$archive" \
        || err "could not download ${RELEASES}/download/v${version}/${archive}"

    actual=$(sha256_of "$tmp/$archive")
    if [ "$actual" != "$expected" ]; then
        err "checksum mismatch for ${archive}.
       expected ${expected}
       got      ${actual}
       Nothing has been extracted or installed. This means the download was
       corrupted or tampered with; do not retry blindly."
    fi
    info "checksum   ok"

    # Only now, verified, is anything unpacked.
    tar xzf "$tmp/$archive" -C "$tmp"
    binary="$tmp/autonet-${version}-${target}/autonet"
    [ -f "$binary" ] || err "the archive did not contain autonet where expected (${binary#$tmp/})."
    chmod +x "$binary"

    dest=$(choose_dir)
    install_to "$binary" "$dest"

    say ""
    say "Installed ${dest}/autonet (${version})."
    quarantine_note "$dest/autonet"
    path_note "$dest"
    say ""
    say "Try:  autonet status"
}

# --- where to put it ---------------------------------------------------------
choose_dir() {
    if [ -n "${AUTONET_INSTALL_DIR:-}" ]; then
        mkdir -p "$AUTONET_INSTALL_DIR" 2>/dev/null \
            || err "AUTONET_INSTALL_DIR=${AUTONET_INSTALL_DIR} could not be created."
        [ -w "$AUTONET_INSTALL_DIR" ] || err "AUTONET_INSTALL_DIR=${AUTONET_INSTALL_DIR} is not writable."
        printf '%s' "$AUTONET_INSTALL_DIR"
        return
    fi
    # /usr/local/bin only when it is already writable by this user; making it
    # writable is a sudo this script does not take for you.
    if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        printf '%s' /usr/local/bin
        return
    fi
    mkdir -p "$HOME/.local/bin" || err "could not create ${HOME}/.local/bin."
    printf '%s' "$HOME/.local/bin"
}

# `[ -r /dev/tty ]` is the wrong question: /dev/tty passes its permission bits
# even with no controlling terminal, and it is the open that fails there. So try
# the open, both ways round.
#
# The subshells are load-bearing. A redirection error on a *special* built-in
# makes the shell exit, and `:` is one — so `{ : < /dev/tty; } 2>/dev/null`
# terminates the installer instead of evaluating false, with the redirect hiding
# why. dash is strict here and bash is not, and `curl | sh` gets dash on Debian
# and Ubuntu. A subshell confines the exit.
tty_available() {
    ( : < /dev/tty ) 2>/dev/null && ( : > /dev/tty ) 2>/dev/null
}

install_to() {
    from=$1
    dir=$2
    to="$dir/autonet"

    if [ -e "$to" ]; then
        existing=$("$to" --version 2>/dev/null || echo "unknown version")
        if [ "${AUTONET_FORCE:-}" = 1 ]; then
            info "replacing   ${to} (${existing})"
        elif tty_available; then
            # Read from the terminal, not stdin: under `curl | sh` stdin is the
            # script itself, so `read` would eat the rest of this file.
            printf '  %s already exists (%s). Replace it? [y/N] ' "$to" "$existing" > /dev/tty
            read -r reply < /dev/tty || reply=""
            case "$reply" in
                y | Y | yes | YES) ;;
                *) err "left ${to} alone. Nothing was installed." ;;
            esac
        else
            err "${to} already exists (${existing}), and there is no terminal to
       ask on. Re-run with AUTONET_FORCE=1 to replace it, or set
       AUTONET_INSTALL_DIR to somewhere else."
        fi
    fi

    # Write beside the target and rename, so a full disk or a denied write
    # cannot leave a half-written binary where a working one used to be.
    cp "$from" "$to.new" || err "could not write to ${dir}."
    chmod 755 "$to.new"
    mv -f "$to.new" "$to"
}

# --- macOS quarantine --------------------------------------------------------
#
# Printed only when the attribute is actually present. curl and wget do not set
# com.apple.quarantine — browsers, Mail and the App Store do — so on the normal
# path there is nothing to say, and saying it anyway is noise in front of every
# macOS user. The command is printed for a person to run rather than run here:
# clearing a security attribute silently is a habit worth not teaching.
quarantine_note() {
    [ "$(uname -s)" = Darwin ] || return 0
    have xattr || return 0
    xattr -p com.apple.quarantine "$1" >/dev/null 2>&1 || return 0
    say ""
    say "  This binary is quarantined by Gatekeeper, and macOS will refuse to run"
    say "  it until that is cleared. AutoNet is not signed or notarized, so this"
    say "  is expected rather than a sign that something is wrong. To clear it:"
    say ""
    say "      xattr -d com.apple.quarantine $1"
}

# --- PATH --------------------------------------------------------------------
path_note() {
    case ":${PATH}:" in
        *":$1:"*) return 0 ;;
    esac
    say ""
    say "  ${1} is not on your PATH. Add it:"
    say ""
    say "      export PATH=\"${1}:\$PATH\""
    say ""
    say "  and put that line in your shell's startup file to make it stick."
}

main "$@"

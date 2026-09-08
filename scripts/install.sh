#!/bin/sh
#
# AutoNet installer, for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/phravins/AUTONET/main/scripts/install.sh | sh
#
# Windows is not handled here; see packaging/scoop/autonet.json.
#
# Written as POSIX sh on purpose. In `curl ... | sh` the shebang is never read —
# the interpreter is whatever the user piped to — so `set -o pipefail`, arrays
# and `[[` are all unavailable no matter what the first line says.
#
# What it will not do:
#
#   * It never calls sudo. AutoNet needs no privileges to run and its installer
#     should need none either. If /usr/local/bin is not writable it installs to
#     ~/.local/bin and says so, rather than asking a pipe-to-shell script for
#     your password.
#   * It never extracts an archive it has not checksummed. Verification happens
#     while the download is still in a temporary directory, because a checksum
#     confirmed after the binary is already on your PATH has confirmed nothing.
#   * It never overwrites an existing autonet without asking.
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
# The four Unix triples here are exactly the four that
# .github/workflows/release.yml builds. Anything else is refused by name rather
# than guessed at, because the alternative is downloading a 404 page and trying
# to execute it.
detect_target() {
    kernel=$(uname -s)
    machine=$(uname -m)

    case "$kernel" in
        Linux)  os="unknown-linux-gnu" ;;
        Darwin) os="apple-darwin" ;;
        *) err "unsupported operating system: $kernel (AutoNet ships Linux and macOS builds; on Windows use Scoop)" ;;
    esac

    case "$machine" in
        x86_64 | amd64)  arch="x86_64" ;;
        aarch64 | arm64) arch="aarch64" ;;
        *) err "unsupported architecture: $machine (AutoNet ships x86_64 and aarch64 builds)" ;;
    esac

    # The Linux builds link against glibc. On a musl system — Alpine, and the
    # many containers built from it — the binary exists, downloads, passes its
    # checksum and then dies with a confusing "not found" from the loader. Worth
    # catching here, where the message can say why.
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
# If neither is present the install is abandoned. Installing an unverified
# binary "just this once" is the failure this whole step exists to prevent, and
# a fallback that skips it silently would make the earlier verification
# theatre rather than a guarantee.
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

    # SHA256SUMS is fetched first and does double duty: it is the integrity
    # check, and — because release.yml puts the version in every filename — it
    # is also how the version is discovered. GitHub resolves /latest/download/
    # server-side, so this needs no API call and so cannot be rate-limited, and
    # there is no second source of truth to disagree with the first.
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

    # The line for this platform, which also settles the version and the
    # archive's extension without either being assumed.
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

    # Only now, with the archive verified, is anything unpacked. The tarballs
    # release.yml builds contain a single directory named after the archive.
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
    # /usr/local/bin only when it is already writable by this user. Making it
    # writable is a sudo away and this script does not take that step for you.
    if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        printf '%s' /usr/local/bin
        return
    fi
    mkdir -p "$HOME/.local/bin" || err "could not create ${HOME}/.local/bin."
    printf '%s' "$HOME/.local/bin"
}

# `[ -r /dev/tty ]` is not the question. /dev/tty exists and is readable by its
# permission bits even in a session with no controlling terminal, and the open
# is what fails there — with a raw "No such device or address" from the shell
# rather than anything this script could explain. So try the open, both ways
# round, and let that be the answer.
#
# The subshells are load-bearing, and this is subtle enough to be worth the
# paragraph. POSIX says a redirection error on a *special* built-in makes the
# shell exit, and `:` is a special built-in — so `{ : < /dev/tty; } 2>/dev/null`
# does not evaluate to false when the open fails, it terminates the installer,
# and the 2>/dev/null hides the reason. bash is lenient here and dash is not,
# which is the worst combination: on Debian and Ubuntu `/bin/sh` is dash, so
# `curl | sh` gets the strict one while anyone testing with bash sees it work.
# Running each open in a subshell confines the exit to the subshell.
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
            # Read from the terminal, not from stdin: under `curl | sh` stdin is
            # the script itself, so `read` there would consume the rest of this
            # file rather than wait for an answer.
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

    # Copy to a neighbouring name and rename, so that a full disk or a denied
    # write cannot leave a half-written binary where a working one used to be.
    cp "$from" "$to.new" || err "could not write to ${dir}."
    chmod 755 "$to.new"
    mv -f "$to.new" "$to"
}

# --- macOS quarantine --------------------------------------------------------
#
# AutoNet is not signed and not notarized, so Gatekeeper is a real part of the
# experience on macOS and worth being straight about.
#
# This script does NOT strip the quarantine attribute for you, for two reasons.
# The first is that in this path there is nothing to strip: com.apple.quarantine
# is set by applications that opt into it — browsers, Mail, the App Store — and
# curl and wget do not, so a binary this script downloaded normally arrives
# unquarantined and `xattr -d` would be a no-op run for superstition.
#
# The second is that the case where it IS set is the case where it should not be
# removed silently: someone downloaded the tarball in a browser. Having a script
# clear a security attribute without comment is a habit worth not teaching, so
# the attribute is checked for, and if it is genuinely there the exact command
# is printed for a person to run deliberately.
quarantine_note() {
    [ "$(uname -s)" = Darwin ] || return 0
    say ""
    if have xattr && xattr -p com.apple.quarantine "$1" >/dev/null 2>&1; then
        say "  This binary is quarantined by Gatekeeper, and macOS will refuse to run"
        say "  it until that is cleared. AutoNet is not signed or notarized, so this"
        say "  is expected rather than a sign that something is wrong. To clear it:"
        say ""
        say "      xattr -d com.apple.quarantine $1"
    else
        say "  Note: AutoNet is not signed or notarized. Nothing is quarantined here,"
        say "  because curl does not set that attribute — but if you ever download a"
        say "  release tarball with a browser instead, macOS will block it until you"
        say "  run:  xattr -d com.apple.quarantine /path/to/autonet"
    fi
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

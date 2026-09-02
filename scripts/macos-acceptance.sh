#!/usr/bin/env bash
#
# Milestone 2a hardware acceptance: run AutoNet beside the operating system's
# own answer, under one named network condition, and keep everything.
#
#   scripts/macos-acceptance.sh wifi
#   scripts/macos-acceptance.sh ethernet
#   scripts/macos-acceptance.sh both
#   scripts/macos-acceptance.sh vpn
#
# Everything lands in acceptance/<scenario>/ (gitignored), plus one fixture in
# tests/fixtures/macos-real-<scenario>.json. Record the results in
# docs/milestone-2a-acceptance.md — the checklist is the deliverable; this
# script only gathers the evidence for it.
#
# Deliberately NOT `set -e`. A failing live test, a `no address` exit from
# `autonet ip`, a missing `networksetup` — each of those is a *result*, and a
# run that aborts on the first one tells us less than a run that finishes and
# reports it. Every exit code is recorded in SUMMARY.txt instead.
#
# Deliberately no `jq`, because macOS does not ship it. The human-readable
# tables are the at-a-glance view; the --json files are for reading afterwards.

set -u

scenario="${1-}"
case "$scenario" in
  wifi | ethernet | both | vpn) ;;
  *)
    echo "usage: $0 <wifi|ethernet|both|vpn>" >&2
    echo >&2
    echo "  wifi      Wi-Fi associated, nothing else up" >&2
    echo "  ethernet  a wire, Wi-Fi off" >&2
    echo "  both      Wi-Fi and Ethernet up at once" >&2
    echo "  vpn       a tunnel up over whatever else is connected" >&2
    exit 64
    ;;
esac

repo="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo/acceptance/$scenario"
fixture="$repo/tests/fixtures/macos-real-$scenario.json"
summary="$out/SUMMARY.txt"

mkdir -p "$out" || exit 1
cd "$repo" || exit 1

# --------------------------------------------------------------------------
# Helpers
# --------------------------------------------------------------------------

# Names and exit codes, in order, for SUMMARY.txt. Two parallel arrays rather
# than an associative one so this still runs under the bash 3.2 that ships with
# macOS.
step_names=()
step_codes=()

# Run a command, tee its combined output to a file, and remember what it
# returned. Combined, because a backend error on stderr belongs beside the
# stdout it failed to produce.
record() {
  local file="$1" name="$2"
  shift 2
  {
    echo "\$ $*"
    echo
  } >"$out/$file"
  "$@" >>"$out/$file" 2>&1
  local code=$?
  step_names+=("$name")
  step_codes+=("$code")
  printf '  %-34s exit %d\n' "$name" "$code"
  return $code
}

# Same, for the operating system's own tools: absent is a note, not a failure.
# It is what lets this script be smoke-tested on Linux, where none of them
# exist — a run there proves the plumbing works and nothing about macOS.
record_os() {
  local file="$1" name="$2"
  shift 2
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "$1: not available on this system" >"$out/$file"
    step_names+=("$name")
    step_codes+=("n/a")
    printf '  %-34s not available\n' "$name"
    return 0
  fi
  record "$file" "$name" "$@"
}

section() {
  echo
  echo "=== $1 ==="
}

# --------------------------------------------------------------------------
# 00 — what machine, what tree
# --------------------------------------------------------------------------

os="$(uname -s)"

{
  echo "scenario:   $scenario"
  echo "date:       $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "uname:      $(uname -a)"
  command -v sw_vers >/dev/null 2>&1 && sw_vers
  echo "commit:     $(git rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "dirty:      $(test -n "$(git status --porcelain 2>/dev/null)" && echo yes || echo no)"
  command -v rustc >/dev/null 2>&1 && echo "rustc:      $(rustc --version)"
} >"$out/00-environment.txt"

echo "AutoNet acceptance — scenario '$scenario' — output in acceptance/$scenario/"
cat "$out/00-environment.txt"

if [ "$os" != "Darwin" ]; then
  echo
  echo "!!  NOT macOS (uname -s = $os)."
  echo "!!  This run exercises the script, not the macOS backend. Nothing it"
  echo "!!  produces is evidence for Milestone 2a. No fixture will be written."
fi

# --------------------------------------------------------------------------
# 10 — the operating system's own answer, captured first
# --------------------------------------------------------------------------
# First, so that if the network changes mid-run the OS view is the one taken
# *before* AutoNet's, and a disagreement can be read as "the network moved"
# rather than silently blamed on the parser.

section "the OS's own view"
record_os 10-os-ifconfig.txt "ifconfig -a" ifconfig -a
record_os 11-os-netstat-inet.txt "netstat -rnf inet" netstat -rnf inet
record_os 12-os-netstat-inet6.txt "netstat -rnf inet6" netstat -rnf inet6
record_os 13-os-service-order.txt "networksetup service order" \
  networksetup -listnetworkserviceorder
record_os 14-os-scutil-nwi.txt "scutil --nwi" scutil --nwi

# --------------------------------------------------------------------------
# 20 — AutoNet
# --------------------------------------------------------------------------
# Built once and invoked as a binary, so that --json output is exactly the
# document and not a document with cargo's progress lines in front of it.

section "building"
if ! cargo build --release -p autonet-cli; then
  echo "the CLI did not build — stopping, since nothing below would mean anything" >&2
  exit 1
fi
autonet="$repo/target/release/autonet"

section "AutoNet's answer"
record 20-autonet-status.txt "status" "$autonet" status
record 21-autonet-status.json "status --json -v" "$autonet" status --json -v
record 22-autonet-ip.txt "ip" "$autonet" ip
record 23-autonet-ip.json "ip --json" "$autonet" ip --json
record 24-autonet-interfaces.txt "interfaces -v" "$autonet" interfaces -v
record 25-autonet-interfaces.json "interfaces --json -v" "$autonet" interfaces --json -v
record 26-autonet-routes.txt "routes -v" "$autonet" routes -v
record 27-autonet-routes.json "routes --json -v" "$autonet" routes --json -v

# --------------------------------------------------------------------------
# 30 — the live tests
# --------------------------------------------------------------------------
# --ignored runs exactly the live set; --nocapture is not optional here. Several
# of these tests can pass without proving anything — the RTA_IFP cross-check
# passes vacuously on a table carrying no RTA_IFP, and the same-kind test skips
# itself on a single-NIC machine — and the counts they print are the only way to
# tell "verified" from "had nothing to look at".

section "live tests (the whole point of the exercise)"
record 30-live-tests.txt "cargo test -- --ignored" \
  cargo test -p autonet-platform -- --ignored --nocapture

# --------------------------------------------------------------------------
# 40 — the capture
# --------------------------------------------------------------------------

section "capture"
if [ "$os" = "Darwin" ]; then
  if cargo run -q -p autonet-platform --example capture >"$out/40-capture.json" 2>"$out/40-capture.err"; then
    cp "$out/40-capture.json" "$fixture"
    step_names+=("capture -> macos-real-$scenario.json")
    step_codes+=("0")
    echo "  wrote tests/fixtures/macos-real-$scenario.json"
    echo "  NOTE: a committed capture publishes this machine's interface names,"
    echo "        addresses and prefixes. MACs are stripped; addresses are not,"
    echo "        because they are what the fixture is for. Read it before"
    echo "        committing it."
  else
    step_names+=("capture")
    step_codes+=("failed")
    echo "  capture FAILED — see acceptance/$scenario/40-capture.err"
  fi
else
  echo "  skipped: a capture from $os would be mislabelled as macos-real-*"
fi

# --------------------------------------------------------------------------
# Summary
# --------------------------------------------------------------------------

{
  echo "AutoNet Milestone 2a acceptance — scenario '$scenario'"
  echo
  cat "$out/00-environment.txt"
  echo
  echo "--- exit codes ---"
  i=0
  while [ $i -lt ${#step_names[@]} ]; do
    printf '%-38s %s\n' "${step_names[$i]}" "${step_codes[$i]}"
    i=$((i + 1))
  done
  echo
  echo "--- side by side: what each one says the machine looks like ---"
  echo
  echo "### autonet interfaces -v"
  cat "$out/24-autonet-interfaces.txt"
  echo
  echo "### autonet routes -v"
  cat "$out/26-autonet-routes.txt"
  echo
  echo "### autonet ip"
  cat "$out/22-autonet-ip.txt"
  echo
  echo "### networksetup -listnetworkserviceorder"
  cat "$out/13-os-service-order.txt"
  echo
  echo "### netstat -rnf inet"
  cat "$out/11-os-netstat-inet.txt"
  echo
  echo "--- live test results ---"
  # The per-test ok/FAILED lines and the counts the tests print, without the
  # compile chatter.
  grep -E '^test |^running |result:|cross-checked|checked |skipped:|note:' \
    "$out/30-live-tests.txt" 2>/dev/null || echo "(no test output captured)"
} >"$summary"

section "done"
echo "Full write-up: acceptance/$scenario/SUMMARY.txt"
echo "Now fill in the '$scenario' column of docs/milestone-2a-acceptance.md."
echo
echo "Anything wrong is a finding against Task 3, 4 or 5 — the doc's"
echo "trace-to-task table says which. Do not fix it here."

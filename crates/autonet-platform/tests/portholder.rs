//! Port attribution, checked against a socket this test really holds.
//!
//! Deliberately **not** `#[ignore]`d, unlike `live.rs`: the test process binds
//! the socket itself, so it is its own ground truth and needs nothing from the
//! machine's network. CI runs `cargo test --workspace` on Linux, macOS and
//! Windows, which makes this the one part of the port work that is exercised
//! on all three rather than reasoned about for two.
//!
//! Every listener is bound on port 0, so the kernel picks a free port and
//! parallel test binaries cannot collide.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

use autonet_platform::{port_holder, PortHolder};

/// Bind a listener on an OS-chosen port, and report where it landed.
fn listen(address: IpAddr) -> Option<(TcpListener, u16)> {
    let listener = TcpListener::bind(SocketAddr::new(address, 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    Some((listener, port))
}

/// What this platform is expected to say about a port we are holding.
///
/// macOS is the odd one out on purpose, and is asserted rather than skipped so
/// that a future macOS backend cannot land without this test noticing.
#[track_caller]
fn assert_held_by_us(holder: &PortHolder) {
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    assert!(
        matches!(holder, PortHolder::Named { pid, .. } if *pid == std::process::id()),
        "expected this process to be named as the holder, got {holder:?}"
    );

    #[cfg(target_os = "macos")]
    assert!(
        matches!(holder, PortHolder::Unsupported { platform } if *platform == "macos"),
        "macOS is expected to decline the question, got {holder:?}"
    );

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    assert!(
        matches!(holder, PortHolder::Unsupported { .. }),
        "{holder:?}"
    );
}

#[test]
fn a_port_this_process_is_listening_on_is_attributed_to_it() {
    let local = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (_listener, port) = listen(local).expect("a loopback listener");

    assert_held_by_us(&port_holder(local, port));
}

#[test]
fn a_wildcard_listener_is_found_when_asking_about_one_address() {
    // The case the CLI's second probe exists for: nothing is bound to
    // 127.0.0.1 specifically, yet 127.0.0.1 is not available.
    let (_listener, port) = listen(IpAddr::V4(Ipv4Addr::UNSPECIFIED)).expect("a wildcard listener");

    assert_held_by_us(&port_holder(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
}

#[test]
fn an_ipv6_listener_is_attributed_the_same_way() {
    let local = IpAddr::V6(Ipv6Addr::LOCALHOST);
    let Some((_listener, port)) = listen(local) else {
        // Some CI containers run without IPv6 at all. Skipping is honest;
        // asserting against a socket that could not be bound would not be.
        eprintln!("skipped: this machine cannot bind ::1");
        return;
    };

    assert_held_by_us(&port_holder(local, port));
}

#[test]
fn a_port_nobody_is_listening_on_is_not_attributed_to_this_process() {
    let local = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (listener, port) = listen(local).expect("a loopback listener");
    drop(listener);

    // Asserted as "no longer ours" rather than "NotListed": the port is free
    // the instant it is released, so another process on a busy machine may
    // legitimately have taken it by the time the lookup runs.
    let holder = port_holder(local, port);
    assert!(
        !matches!(holder, PortHolder::Named { pid, .. } if pid == std::process::id()),
        "a released port is still attributed to this process: {holder:?}"
    );
}

#[test]
fn a_platform_that_cannot_answer_names_itself() {
    // The CLI phrases this variant without knowing which OS it is on, so the
    // name has to come from here. `provider()` makes the same promise.
    let holder = port_holder(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    if let PortHolder::Unsupported { platform } = holder {
        assert_eq!(platform, std::env::consts::OS);
    }
}

//! Is the port already taken, and by whom.
//!
//! `autonet run --port 3000 -- npm start` used to start the command, print a
//! URL, and let the child discover several seconds later that it could not
//! bind. This moves that discovery in front of the spawn.
//!
//! **It warns; it never refuses.** `--port` is a rendering hint — it says what
//! URL to print, not what the command will bind — so a busy port is a very
//! good guess about a problem and not a fact about one. `autonet run --port
//! 3000 -- npm start` may perfectly well start a server on 5173, and AutoNet
//! declining to launch what it was asked to launch on the strength of that
//! guess would be plainly wrong.
//!
//! The evidence is a real bind, not a table lookup: the tables answer *who*,
//! and only [`std::net::TcpListener`] answers *whether*. Two probes, because
//! "in use" is not one question. A holder on `127.0.0.1:3000` leaves
//! `192.168.1.20:3000` bindable and vice versa, so a single probe of the
//! selected address reports "free" for a port a server binding `0.0.0.0` cannot
//! have. Measured on this machine across four holder arrangements.
//!
//! Kept out of `spawn` because `doctor` asks the same question in M3 Task 3.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

use autonet_platform::{port_holder, PortHolder};

/// What a real bind attempt said about a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The bind succeeded, and was released again immediately.
    Free,

    /// The bind was refused with `EADDRINUSE`. The only positive evidence.
    InUse,

    /// The bind failed for a reason that says nothing about the port.
    ///
    /// Load-bearing: `--port 80` as an ordinary user is refused with `EACCES`
    /// long before anything checks whether port 80 is free, and reporting that
    /// as "in use" would be simply false.
    Undetermined,
}

/// What two probes and a table lookup found out about one port.
#[derive(Debug)]
pub struct PortCheck {
    /// The address AutoNet selected.
    pub address: IpAddr,

    /// The wildcard in that address's family, which a server is as likely to
    /// bind as the selected address itself.
    pub wildcard: IpAddr,

    /// Whether the selected address is bindable.
    pub on_address: Availability,

    /// Whether the wildcard is bindable.
    pub on_wildcard: Availability,

    /// Who holds it, as far as this platform will say.
    pub holder: PortHolder,
}

/// Probe a port and, if something holds it, find out what.
pub fn inspect(address: IpAddr, port: u16) -> PortCheck {
    let wildcard = wildcard_for(address);
    let on_address = availability(address, port);
    let on_wildcard = availability(wildcard, port);

    // Asked about whichever probe found a collision, so the answer describes
    // the socket actually in the way. Skipped entirely when nothing is: the
    // lookup walks every process on the machine.
    let holder = if on_address == Availability::InUse {
        port_holder(address, port)
    } else if on_wildcard == Availability::InUse {
        port_holder(wildcard, port)
    } else {
        PortHolder::NotListed
    };

    PortCheck {
        address,
        wildcard,
        on_address,
        on_wildcard,
        holder,
    }
}

/// Ask the kernel by binding, then let go.
///
/// The listener is dropped at the end of this function, so the port is held
/// for microseconds and never while the child is starting.
///
/// This is inherently a time-of-check/time-of-use answer: the port can be
/// taken between here and the spawn. That is unavoidable for anything short of
/// binding on the child's behalf and handing it the descriptor, which is a
/// different feature.
fn availability(address: IpAddr, port: u16) -> Availability {
    match TcpListener::bind(SocketAddr::new(address, port)) {
        Ok(_) => Availability::Free,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Availability::InUse,
        // EACCES on a privileged port, EADDRNOTAVAIL for an address this
        // machine no longer holds. Neither says the port is busy.
        Err(_) => Availability::Undetermined,
    }
}

/// The any-address of the same family.
fn wildcard_for(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    }
}

/// The warning to print, or nothing if there is nothing to say.
///
/// Pure, so the wording is testable without a socket: this is the whole
/// user-visible product of the module, and each branch describes a materially
/// different situation that deserves different advice.
pub fn describe(check: &PortCheck, port: u16) -> Option<String> {
    let PortCheck {
        address,
        wildcard,
        on_address,
        on_wildcard,
        holder,
    } = check;

    let (situation, advice) = match (on_address, on_wildcard) {
        (Availability::InUse, _) => (
            format!("port {port} is already in use on {address}."),
            "Starting the command anyway: --port renders URLs, it does not choose what the \
             command binds."
                .to_owned(),
        ),
        // Free here, busy there: a server binding one address will start and a
        // server binding every address will not, which is worth separating.
        (Availability::Free, Availability::InUse) => (
            format!("port {port} is free on {address}, but held elsewhere on this machine."),
            format!(
                "A command that binds {wildcard} will fail; one that binds {address} will not."
            ),
        ),
        // No evidence of a collision, so nothing to report. `Undetermined` on
        // both probes lands here on purpose.
        _ => return None,
    };

    Some(match holder_note(holder) {
        Some(note) => format!("{situation} {note} {advice}"),
        None => format!("{situation} {advice}"),
    })
}

/// How this platform names the holder, if it will.
///
/// Every variant is phrased, including the ones that decline: a user comparing
/// output across machines should be told that macOS says less, not left to
/// infer it from a shorter message.
pub(crate) fn holder_note(holder: &PortHolder) -> Option<String> {
    match holder {
        PortHolder::Named { pid, name } => Some(format!("It is held by {name} (pid {pid}).")),
        PortHolder::Unnamed { pid } => Some(format!("It is held by pid {pid}.")),
        PortHolder::OtherUser { uid } => Some(format!(
            "It is held by a process owned by uid {uid}, which this user may not inspect."
        )),
        // The bind said busy and the table said nothing: the two are separate
        // calls, and a socket can come or go between them. Better to say
        // nothing than to explain a race.
        PortHolder::NotListed => None,
        PortHolder::Unsupported { platform } => Some(format!(
            "AutoNet cannot identify which process holds a port on {platform}."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(on_address: Availability, on_wildcard: Availability, holder: PortHolder) -> PortCheck {
        PortCheck {
            address: "192.168.1.20".parse().unwrap(),
            wildcard: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            on_address,
            on_wildcard,
            holder,
        }
    }

    fn named() -> PortHolder {
        PortHolder::Named {
            pid: 4821,
            name: "node".to_owned(),
        }
    }

    #[test]
    fn a_free_port_says_nothing_at_all() {
        let quiet = check(
            Availability::Free,
            Availability::Free,
            PortHolder::NotListed,
        );
        assert!(describe(&quiet, 3000).is_none());
    }

    #[test]
    fn a_port_we_may_not_bind_is_not_reported_as_busy() {
        // `--port 80` as an ordinary user. EACCES is not evidence.
        let denied = check(
            Availability::Undetermined,
            Availability::Undetermined,
            PortHolder::NotListed,
        );
        assert!(describe(&denied, 80).is_none());
    }

    #[test]
    fn a_busy_port_names_the_holder_and_says_the_command_still_starts() {
        let busy = check(Availability::InUse, Availability::InUse, named());
        let message = describe(&busy, 3000).expect("a warning");

        assert!(message.contains("port 3000 is already in use on 192.168.1.20"));
        assert!(message.contains("node (pid 4821)"));
        // The promise that this is a warning and not a refusal.
        assert!(message.contains("Starting the command anyway"));
    }

    #[test]
    fn a_port_free_here_but_held_elsewhere_gets_its_own_advice() {
        let split = check(Availability::Free, Availability::InUse, named());
        let message = describe(&split, 3000).expect("a warning");

        assert!(message.contains("free on 192.168.1.20"));
        assert!(message.contains("binds 0.0.0.0 will fail"));
        assert!(message.contains("binds 192.168.1.20 will not"));
        // Not the other branch's advice, which would contradict this one.
        assert!(!message.contains("already in use"));
    }

    #[test]
    fn an_unidentified_holder_still_reports_the_collision() {
        let busy = check(
            Availability::InUse,
            Availability::InUse,
            PortHolder::NotListed,
        );
        let message = describe(&busy, 3000).expect("a warning");

        assert!(message.contains("already in use"));
        assert!(!message.contains("held by"));
        // No double space where the missing clause was.
        assert!(!message.contains("  "), "{message}");
    }

    #[test]
    fn a_platform_that_cannot_name_the_holder_says_so_rather_than_going_quiet() {
        // The whole point: macOS must not merely print less than Linux does.
        let busy = check(
            Availability::InUse,
            Availability::InUse,
            PortHolder::Unsupported { platform: "macos" },
        );
        let message = describe(&busy, 3000).expect("a warning");

        assert!(message.contains("cannot identify which process holds a port on macos"));
    }

    #[test]
    fn a_holder_owned_by_another_account_is_reported_as_a_permission_limit() {
        let busy = check(
            Availability::InUse,
            Availability::InUse,
            PortHolder::OtherUser { uid: 0 },
        );
        let message = describe(&busy, 3000).expect("a warning");

        assert!(message.contains("uid 0"));
        assert!(message.contains("may not inspect"));
    }

    #[test]
    fn a_nameless_pid_is_still_worth_printing() {
        let busy = check(
            Availability::InUse,
            Availability::InUse,
            PortHolder::Unnamed { pid: 4821 },
        );
        assert!(describe(&busy, 3000)
            .expect("a warning")
            .contains("held by pid 4821"));
    }

    #[test]
    fn the_wildcard_matches_the_family_of_the_address() {
        assert_eq!(
            wildcard_for("192.168.1.20".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        );
        assert_eq!(
            wildcard_for("fe80::1".parse().unwrap()),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        );
    }

    #[test]
    fn a_port_this_test_is_holding_is_detected_as_in_use() {
        // Against a real socket, not a fixture: the bind probe is the one part
        // of this module that a mock could not honestly stand in for.
        let local = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let held = TcpListener::bind(SocketAddr::new(local, 0)).expect("a loopback listener");
        let port = held.local_addr().expect("a bound address").port();

        assert_eq!(availability(local, port), Availability::InUse);

        let found = inspect(local, port);
        assert_eq!(found.on_address, Availability::InUse);
        assert!(
            describe(&found, port)
                .expect("a warning")
                .contains("already in use"),
            "a really-bound port produced no warning"
        );

        drop(held);
        // And free again once released, so the probe is not simply always busy.
        assert_eq!(availability(local, port), Availability::Free);
    }

    #[test]
    fn a_wildcard_holder_is_seen_from_the_specific_address() {
        // The second probe's reason for existing, against a real socket.
        let held = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
            .expect("a wildcard listener");
        let port = held.local_addr().expect("a bound address").port();

        let found = inspect(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        assert_eq!(found.on_wildcard, Availability::InUse);
        assert!(describe(&found, port).is_some());
    }
}

//! The one place that answers "what URL reaches this machine right now".
//!
//! Six call sites used to build that string independently — `status` in text
//! and in JSON, `ip` in both, `AUTONET_URL` in [`crate::spawn`], and
//! [`crate::advertise`]'s opening block. Each was correct, and each was a
//! separate opportunity to be wrong about IPv6 bracketing or about which host
//! belongs in the string. They now all route through here.
//!
//! # The mDNS swap point
//!
//! [`network_url`]'s `name` parameter is the whole design. A `.local` name is
//! only a better answer than an IP address *while something is publishing it*,
//! and the only thing that publishes one is `autonet advertise`, which holds
//! the responder for as long as it runs. So the caller says whether a name is
//! live; this module does not guess from configuration.
//!
//! That distinction is not pedantry. `hostname.enabled = true` in a config file
//! means "this machine may advertise", not "this machine is advertising right
//! now". Encoding `http://laptop-autonet.local:3000` into a QR code on the
//! strength of a config setting would hand a phone a name nothing is answering
//! — a code that scans perfectly and then fails to load, which is worse than
//! the raw address it replaced. See
//! [ADR 0003](../../../docs/adr/0003-qr-code-contents.md).

use autonet_core::model::Family;
use autonet_core::select::SelectedAddress;

/// The scheme every URL AutoNet renders uses.
///
/// A constant rather than a parameter because nothing in the tool has any way
/// to know whether the server behind the port speaks TLS. Guessing `https`
/// from a port number would be a policy decision dressed as a convenience.
const SCHEME: &str = "http";

/// The URL another device on the network can open.
///
/// `name` is `Some` only when a responder **in this process** is publishing
/// that name right now — which today means [`crate::advertise`] and nothing
/// else. Everything with no responder passes `None` and gets the selected
/// address, which is true for as long as the machine holds it.
///
/// **This is the mDNS swap point.** An `autonet advertise --qr` would pass
/// `Some(&host)` and get the `.local` URL in the code, because there the name
/// is live. No other line has to change.
pub(crate) fn network_url(selected: &SelectedAddress, port: u16, name: Option<&str>) -> String {
    match name {
        // Already a hostname, so it needs no IPv6 bracketing and must not be
        // put through `SelectedAddress::url`, which formats an address.
        Some(host) => format!("{SCHEME}://{host}:{port}"),
        None => selected.url(port, SCHEME),
    }
}

/// The URL a browser **on this machine** would open.
///
/// Kept separate from [`network_url`] rather than derived from it, because
/// conflating the two is the mistake AutoNet exists to prevent: `127.0.0.1` is
/// the answer that looks right on the developer's screen and is useless on
/// everyone else's.
pub(crate) fn local_url(family: Family, port: u16) -> String {
    let host = match family {
        Family::V4 => "127.0.0.1",
        // Bracketed, for the same reason `SelectedAddress::url_host` brackets:
        // `http://::1:3000` has no unambiguous reading.
        Family::V6 => "[::1]",
    };
    format!("{SCHEME}://{host}:{port}")
}

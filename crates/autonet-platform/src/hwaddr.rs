//! Rendering hardware addresses, shared by every backend.
//!
//! Each operating system hands us the bytes differently — netlink puts them in
//! an `IFLA_ADDRESS` attribute, `getifaddrs` puts them inside an `AF_LINK`
//! `sockaddr_dl` — but what counts as a *reportable* MAC is a decision about
//! AutoNet's output, not about any kernel. Keeping it in one place means the
//! Linux and macOS backends cannot drift into disagreeing about it.

/// Render a hardware address, or `None` if it is absent or meaningless.
///
/// Only 6-byte addresses are reported. Loopback and many tunnel devices carry
/// an all-zero or zero-length address, which is noise rather than information,
/// and InfiniBand's 20-byte addresses are not a MAC in any useful sense.
pub(crate) fn format_mac(bytes: &[u8]) -> Option<String> {
    if bytes.len() != 6 || bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs_are_lowercase_colon_separated() {
        assert_eq!(
            format_mac(&[0x02, 0x0a, 0xff, 0x00, 0x1b, 0xc3]).as_deref(),
            Some("02:0a:ff:00:1b:c3")
        );
    }

    #[test]
    fn meaningless_hardware_addresses_are_dropped() {
        assert_eq!(format_mac(&[0; 6]), None, "all-zero (loopback, tun)");
        assert_eq!(format_mac(&[]), None, "absent");
        assert_eq!(format_mac(&[0; 20]), None, "InfiniBand");
    }
}

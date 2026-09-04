//! Parsing `/proc/net/tcp` and `/proc/net/tcp6`.
//!
//! Kept separate from the lookup that uses it because the byte-order handling
//! below is the part that goes quietly wrong, and a pure function over a string
//! can be tested exhaustively without a live socket.
//!
//! The trap: **the address and the port in the same token use opposite
//! conventions.** `0100007F:1F90` is `127.0.0.1:8080`. The address is a
//! `__be32` printed as a host-order `u32`, so its bytes come out reversed on a
//! little-endian machine, while the port is printed in its natural order.
//! IPv6 is four such words, so each four-byte group reverses independently
//! rather than the sixteen bytes reversing as a whole.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `TCP_LISTEN` from `include/net/tcp_states.h`, as the kernel prints it.
///
/// Restated rather than imported: it is a kernel-internal enum with no `libc`
/// definition, and only its textual form is ever compared here.
const LISTEN: &str = "0A";

/// One listening socket, as `/proc/net/tcp` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listener {
    /// The address it is bound to, unspecified for a wildcard bind.
    pub(crate) address: IpAddr,
    /// The port it is listening on.
    pub(crate) port: u16,
    /// The account that opened it. Readable for every socket, including ones
    /// whose owning process this user may not inspect.
    pub(crate) uid: u32,
    /// The socket inode, which is what `/proc/<pid>/fd` links name.
    pub(crate) inode: u64,
}

/// Every listening socket in one `/proc/net/tcp` or `/proc/net/tcp6` file.
///
/// The header line is not comment-prefixed, so it cannot be recognised by shape.
/// It is rejected the same way a malformed row is: its state column reads `st`
/// rather than `0A`.
pub(crate) fn listeners(text: &str) -> Vec<Listener> {
    text.lines().filter_map(row).collect()
}

/// Parse one row, or skip it.
fn row(line: &str) -> Option<Listener> {
    let mut fields = line.split_whitespace();

    // Field order is fixed. The queue and timer columns are colon-joined pairs
    // that arrive as a single token each, which is why counting matters more
    // than it looks.
    let _slot = fields.next()?;
    let local = fields.next()?;
    let _remote = fields.next()?;
    if fields.next()? != LISTEN {
        return None;
    }
    let _queues = fields.next()?;
    let _timer = fields.next()?;
    let _retransmits = fields.next()?;
    let uid: u32 = fields.next()?.parse().ok()?;
    let _timeout = fields.next()?;
    let inode: u64 = fields.next()?.parse().ok()?;

    let (address, port) = local.split_once(':')?;
    Some(Listener {
        address: hex_address(address)?,
        // Natural order, unlike the address beside it.
        port: u16::from_str_radix(port, 16).ok()?,
        uid,
        inode,
    })
}

/// Decode the address half of a `local_address` token.
///
/// The length decides the family: eight hex digits for IPv4, thirty-two for
/// IPv6. Anything else is not an address this kernel wrote.
fn hex_address(hex: &str) -> Option<IpAddr> {
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(hex)?))),
        32 => {
            let mut octets = [0u8; 16];
            for (index, group) in octets.chunks_mut(4).enumerate() {
                let start = index * 8;
                group.copy_from_slice(&word(hex.get(start..start + 8)?)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => None,
    }
}

/// One 32-bit group, as the four address bytes it stands for.
///
/// `to_ne_bytes` rather than `to_le_bytes`: the kernel prints the in-memory
/// bytes of a `__be32` read back as a host-order `u32`, so undoing that means
/// reading it back in *host* order too. Hardcoding little-endian would be
/// correct on every machine anyone will run this on and wrong in principle.
fn word(hex: &str) -> Option<[u8; 4]> {
    // `from_str_radix` accepts a leading `+`, which would let a malformed token
    // of the right length through.
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(u32::from_str_radix(hex, 16).ok()?.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real capture from this machine, header included.
    const SAMPLE: &str = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:AA4B 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 56901 1 00000000545782ca 100 0 0 10 0
   1: 00000000:1B9E 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 20715 1 00000000ee0555f2 100 0 0 10 0
   2: 0100007F:8B36 0100007F:D9A2 01 00000000:00000000 00:00000000 00000000  1000        0 71234 1 00000000aabbccdd 100 0 0 10 0
";

    #[test]
    fn the_header_is_not_mistaken_for_a_socket() {
        // It is not comment-prefixed, so nothing but parsing rejects it.
        assert_eq!(listeners(SAMPLE).len(), 2);
    }

    #[test]
    fn an_established_connection_is_not_a_listener() {
        // State 01 is TCP_ESTABLISHED. Reporting it would claim a port is held
        // by whoever last connected out of it.
        assert!(listeners(SAMPLE).iter().all(|l| l.port != 0x8B36));
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn the_address_is_byte_reversed_but_the_port_beside_it_is_not() {
        // The single most likely bug in this file. `0100007F` is 127.0.0.1 and
        // `AA4B` is 43595 -- one is reversed, the other is not. Gated on
        // endianness because the kernel's output is host-order by construction;
        // `word` uses `to_ne_bytes` so the code itself is not gated.
        let found = &listeners(SAMPLE)[0];
        assert_eq!(found.address, "127.0.0.1".parse::<IpAddr>().unwrap());
        assert_eq!(found.port, 43_595);
    }

    #[test]
    fn the_uid_and_inode_columns_are_not_transposed() {
        // Adjacent decimal fields, so a wrong index still parses and still
        // looks plausible. These are the two real values from the capture.
        let found = &listeners(SAMPLE)[0];
        assert_eq!(found.uid, 1000);
        assert_eq!(found.inode, 56901);

        let root = &listeners(SAMPLE)[1];
        assert_eq!(root.uid, 0);
        assert_eq!(root.inode, 20715);
    }

    #[test]
    fn a_wildcard_bind_is_reported_as_unspecified() {
        assert!(listeners(SAMPLE)[1].address.is_unspecified());
    }

    #[test]
    fn an_ipv6_group_reverses_on_its_own_rather_than_the_whole_address() {
        // Written host-order per 32-bit word, as the kernel does. Reversing all
        // sixteen bytes instead would give 1::, not ::1 -- both plausible, only
        // one correct.
        //
        // The first three words are zero; only the last carries the 1, so the
        // token is twenty-four zeros followed by that word as the kernel would
        // print it on this machine.
        let last = u32::from_ne_bytes([0, 0, 0, 1]);
        let local = format!("{:024}{last:08X}", 0);
        assert_eq!(local.len(), 32, "a v6 token is thirty-two hex digits");

        let line = format!(
            "   0: {local}:1F90 00000000000000000000000000000000:0000 0A \
             00000000:00000000 00:00000000 00000000  1000        0 999 1 0 100 0 0 10 0"
        );
        let found = &listeners(&line)[0];
        assert_eq!(found.address, "::1".parse::<IpAddr>().unwrap());
        assert_eq!(found.port, 8080);
    }

    #[test]
    fn an_ipv6_wildcard_is_recognised() {
        let line = "   0: 00000000000000000000000000000000:1B9E \
                    00000000000000000000000000000000:0000 0A 00000000:00000000 \
                    00:00000000 00000000     0        0 20716 1 0 100 0 0 10 0";
        let found = &listeners(line)[0];
        assert!(found.address.is_unspecified());
        assert!(found.address.is_ipv6());
    }

    #[test]
    fn a_truncated_or_scrambled_row_is_skipped_rather_than_guessed_at() {
        assert!(listeners("   0: 0100007F:AA4B 00000000:0000 0A").is_empty());
        assert!(listeners("").is_empty());
        // Right length, not hex: `from_str_radix` would take the leading `+`.
        assert!(listeners(
            "   0: +100007F:AA4B 00000000:0000 0A 0:0 0:0 0 1000 0 1 1 0 100 0 0 10 0"
        )
        .is_empty());
    }
}

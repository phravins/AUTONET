//! The one thing netlink does not tell us: whether a link is Wi-Fi.
//!
//! `IFLA_INFO_KIND` is empty for physical NICs — the kernel reports a driver
//! name for virtual devices (`bridge`, `veth`, `wireguard`) but nothing at all
//! for a real Ethernet port or a real wireless card. So netlink alone cannot
//! distinguish `eno2` from `wlo1`.
//!
//! sysfs can. The cfg80211 subsystem symlinks `phy80211` into every wireless
//! netdev's sysfs directory, so its presence is an exact test rather than a
//! guess at the name. Name prefixes (`wl`, `wlan`) are *not* reliable:
//! systemd's predictable naming produces `wlp3s0` but also `enp0s31f6`, and
//! users rename interfaces freely.
//!
//! Getting this right matters to selection: Ethernet outscores Wi-Fi, so
//! misreading `wlo1` as Ethernet would silently invert the docked-laptop
//! preference.

use std::path::Path;

/// Whether `name` is a wireless device.
///
/// Checks `phy80211` (cfg80211, every modern driver) and falls back to
/// `wireless` (the old Wireless Extensions interface, still present for a few
/// legacy drivers). A missing `/sys` — a container with it unmounted, say —
/// answers "no" rather than failing: being unsure whether a link is Wi-Fi is
/// not a reason to refuse to report an address.
pub(crate) fn is_wireless(name: &str) -> bool {
    // Reject anything that could escape /sys/class/net. Interface names come
    // from the kernel and are well-formed in practice, but this function builds
    // a filesystem path out of one, and a path built from external input should
    // never be able to point somewhere unintended.
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return false;
    }

    let base = Path::new("/sys/class/net").join(name);
    base.join("phy80211").exists() || base.join("wireless").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_attempts_are_rejected_rather_than_followed() {
        assert!(!is_wireless("../../../etc"));
        assert!(!is_wireless("foo/bar"));
        assert!(!is_wireless(""));
    }

    #[test]
    fn a_device_that_does_not_exist_is_not_wireless() {
        assert!(!is_wireless("autonet-no-such-device"));
    }

    #[test]
    fn loopback_is_not_wireless() {
        // `lo` exists on every Linux host, so this asserts the negative case
        // against a real sysfs entry rather than a missing one.
        assert!(!is_wireless("lo"));
    }
}

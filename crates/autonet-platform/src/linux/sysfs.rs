//! The one thing netlink does not tell us: whether a link is Wi-Fi.
//!
//! `IFLA_INFO_KIND` is empty for physical NICs — the kernel names a driver for
//! virtual devices but nothing for a real Ethernet port or wireless card — so
//! netlink alone cannot distinguish `eno2` from `wlo1`.
//!
//! cfg80211 symlinks `phy80211` into every wireless netdev's sysfs directory,
//! so its presence is an exact test. Name prefixes are not: systemd's
//! predictable naming produces both `wlp3s0` and `enp0s31f6`, and users rename
//! interfaces freely. Ethernet outscores Wi-Fi, so a misread would silently
//! invert the docked-laptop preference.

use std::path::Path;

/// Whether `name` is a wireless device.
///
/// Checks `phy80211` (cfg80211) and falls back to `wireless` (the old Wireless
/// Extensions interface, still present for a few legacy drivers). A missing
/// `/sys` answers "no" rather than failing.
pub(crate) fn is_wireless(name: &str) -> bool {
    // This builds a filesystem path out of a name, so reject anything that
    // could escape /sys/class/net even though kernel names are well-formed.
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

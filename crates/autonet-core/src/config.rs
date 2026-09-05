//! Selection configuration and its file/environment loaders.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::model::FamilyPreference;

/// Top-level configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How the selection engine picks an address.
    pub selection: SelectionConfig,
    /// How the CLI renders results.
    pub output: OutputConfig,
    /// The name this machine answers to on the local link.
    pub hostname: HostnameConfig,
}

/// Rules governing address selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct SelectionConfig {
    /// Which address family to favour. Defaults to IPv4.
    pub prefer_family: FamilyPreference,
    /// Allow loopback addresses.
    pub allow_loopback: bool,
    /// Allow `169.254.0.0/16` / `fe80::/10` to be selected. Off by default —
    /// link-local addresses are not usefully routable.
    pub allow_link_local: bool,
    /// Treat VPN and tunnel interfaces as ordinary candidates rather than
    /// penalising them.
    pub allow_vpn: bool,
    /// Treat container and virtualisation interfaces as ordinary candidates.
    pub allow_container: bool,
    /// Consider interfaces the kernel reports as down.
    pub include_down: bool,
    /// Interfaces to reject outright. Entries may end in `*` to match a prefix,
    /// for example `"docker*"`.
    pub exclude_interfaces: Vec<String>,
    /// Interfaces to strongly favour. Same matching rules as
    /// [`SelectionConfig::exclude_interfaces`].
    pub prefer_interfaces: Vec<String>,
    /// Restrict selection to a single named interface.
    pub require_interface: Option<String>,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            prefer_family: FamilyPreference::Ipv4,
            allow_loopback: false,
            allow_link_local: false,
            allow_vpn: false,
            allow_container: false,
            include_down: false,
            exclude_interfaces: Vec::new(),
            prefer_interfaces: Vec::new(),
            require_interface: None,
        }
    }
}

/// Output preferences.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Default rendering format when no flag is given.
    pub format: OutputFormat,
    /// Port used to build example URLs when none is supplied on the command line.
    pub default_port: Option<u16>,
}

/// How the CLI should render its results.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// Human-readable text.
    #[default]
    Text,
    /// Machine-readable JSON.
    Json,
}

/// The name this machine advertises on the local link, and whether it does.
///
/// Present for mDNS: `autonet advertise` publishes a `.local` name pointing at
/// the address the selector chose, so a phone can reach this machine without
/// anyone typing an IP that goes stale the moment the laptop moves. See
/// `docs/adr/0002-mdns-advertisement.md`.
///
/// Separate from [`SelectionConfig`], which decides *which address wins*, and
/// from [`OutputConfig`], which decides *how it is rendered*. A name this
/// machine answers to is neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostnameConfig {
    /// Whether this machine may advertise itself on the local link.
    ///
    /// **Off by default, and deliberately.** Advertising publishes this
    /// machine's name, chosen address and port to every device on the network
    /// segment, unprompted. `docs/architecture.md`'s security posture requires
    /// that any feature making an application LAN-reachable be explicit, so
    /// this is the single switch that says yes — consulted by `autonet
    /// advertise`, and by anything later that could advertise without being
    /// asked directly.
    pub enabled: bool,
    /// The instance name to publish, without the `.local` suffix.
    ///
    /// Unset derives `<hostname>-autonet` from the machine's own hostname. The
    /// suffix is not decoration: on most Linux desktops Avahi already owns
    /// `<hostname>.local`, and on macOS `mDNSResponder` always does. Publishing
    /// the same name means RFC 6762 conflict resolution either renames this
    /// record or leaves two racing, and a client then gets the system
    /// responder's answer — every address on every interface — about half the
    /// time. That is the failure this tool exists to fix.
    pub name: Option<String>,
    /// The DNS-SD service type to advertise, for example `_http._tcp`.
    pub service: String,
}

impl Default for HostnameConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            name: None,
            service: "_http._tcp".to_string(),
        }
    }
}

impl Config {
    /// The default configuration path: `$XDG_CONFIG_HOME/autonet/config.toml`,
    /// then `%APPDATA%\autonet\config.toml`, then
    /// `~/.config/autonet/config.toml`.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        path_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("APPDATA"),
            std::env::var_os("HOME"),
        )
    }

    /// Parse configuration from a TOML string.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::InvalidConfig`] if the text is not valid TOML or
    /// contains a key AutoNet does not recognise.
    pub fn from_toml(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| CoreError::InvalidConfig(e.to_string()))
    }

    /// Load configuration from an explicit path.
    ///
    /// A missing file is an error here: if the user named a path on the command
    /// line, silently ignoring a typo would be worse than failing.
    ///
    /// # Errors
    ///
    /// Returns [`CoreError::ConfigIo`] if the file cannot be read, or
    /// [`CoreError::InvalidConfig`] if its contents are not valid.
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| CoreError::ConfigIo {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml(&text)
    }

    /// Load from the default path, falling back to defaults when absent.
    ///
    /// A malformed file *is* still an error — quietly ignoring bad syntax would
    /// leave the user wondering why their settings do nothing.
    ///
    /// # Errors
    ///
    /// Returns an error only if a configuration file exists but cannot be read
    /// or parsed. An absent file is not an error.
    pub fn load_default() -> Result<Self> {
        match Self::default_path() {
            Some(path) if path.exists() => Self::load_from(&path),
            _ => Ok(Self::default()),
        }
    }

    /// Overlay `AUTONET_*` environment variables onto this configuration.
    ///
    /// Recognised: `AUTONET_FAMILY`, `AUTONET_INTERFACE`,
    /// `AUTONET_EXCLUDE_INTERFACES` (comma-separated), `AUTONET_ALLOW_VPN`,
    /// `AUTONET_ALLOW_CONTAINER`, `AUTONET_ALLOW_LOOPBACK`, `AUTONET_HOSTNAME`.
    ///
    /// There is deliberately **no `AUTONET_ADVERTISE`**. `hostname.enabled` is
    /// the switch that permits publishing this machine on the LAN, and a
    /// security-relevant opt-in that can arrive in an inherited environment is
    /// exactly the "exposes a service as a side effect" shape
    /// `docs/architecture.md`'s security posture rules out. It is settable from
    /// the configuration file and nowhere else.
    ///
    /// # Errors
    ///
    /// Returns an error if a recognised variable holds a value that cannot be
    /// parsed. An unset variable simply leaves the current setting alone.
    pub fn apply_env(&mut self) -> Result<()> {
        if let Some(v) = non_empty_env("AUTONET_FAMILY") {
            self.selection.prefer_family = v.parse()?;
        }
        if let Some(v) = non_empty_env("AUTONET_INTERFACE") {
            self.selection.require_interface = Some(v);
        }
        if let Some(v) = non_empty_env("AUTONET_EXCLUDE_INTERFACES") {
            self.selection.exclude_interfaces = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
        }
        if let Some(v) = parse_bool_env("AUTONET_ALLOW_VPN")? {
            self.selection.allow_vpn = v;
        }
        if let Some(v) = parse_bool_env("AUTONET_ALLOW_CONTAINER")? {
            self.selection.allow_container = v;
        }
        if let Some(v) = parse_bool_env("AUTONET_ALLOW_LOOPBACK")? {
            self.selection.allow_loopback = v;
        }
        if let Some(v) = non_empty_env("AUTONET_HOSTNAME") {
            self.hostname.name = Some(v);
        }
        Ok(())
    }

    /// Load from disk and then overlay the environment — the two lower layers
    /// of the precedence chain, ready for the CLI to apply flags on top.
    ///
    /// # Errors
    ///
    /// Returns an error if the file layer fails to load (see [`Self::load_from`]
    /// and [`Self::load_default`]) or the environment layer fails to parse.
    pub fn load_layered(explicit_path: Option<&Path>) -> Result<Self> {
        let mut config = match explicit_path {
            Some(path) => Self::load_from(path)?,
            None => Self::load_default()?,
        };
        config.apply_env()?;
        Ok(config)
    }
}

/// Pick a config path from the three variables that might name one.
///
/// Taking the values rather than reading them keeps the order testable without
/// mutating the process environment, which `cargo test`'s thread pool makes
/// unsafe to do from one test among many.
///
/// A plain fallback chain rather than a `cfg(target_os)` branch: Linux and macOS
/// do not set `APPDATA`, Windows does not normally set `XDG_CONFIG_HOME`, so the
/// order degrades correctly without the crate needing to know which OS it is on
/// — which `architecture.md` forbids it from knowing.
fn path_from(
    xdg: Option<OsString>,
    appdata: Option<OsString>,
    home: Option<OsString>,
) -> Option<PathBuf> {
    // `XDG_CONFIG_HOME` and `APPDATA` both name a config *root*; `HOME` names a
    // home directory, so only it grows a `.config` segment.
    if let Some(root) = xdg.filter(|v| !v.is_empty()) {
        return Some(config_file(Path::new(&root)));
    }
    if let Some(root) = appdata.filter(|v| !v.is_empty()) {
        return Some(config_file(Path::new(&root)));
    }
    home.filter(|v| !v.is_empty())
        .map(|home| config_file(&Path::new(&home).join(".config")))
}

/// AutoNet's config file inside a configuration root.
fn config_file(root: &Path) -> PathBuf {
    root.join("autonet").join("config.toml")
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn parse_bool_env(key: &str) -> Result<Option<bool>> {
    let Some(raw) = non_empty_env(key) else {
        return Ok(None);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(CoreError::InvalidConfig(format!(
            "{key} must be a boolean, got {raw:?}"
        ))),
    }
}

/// Match an interface name against a pattern.
///
/// Supports exact matches and a single trailing `*` for prefix matching, which
/// covers the realistic cases (`docker*`, `br-*`) without pulling in a glob
/// dependency or surprising anyone with full regex semantics.
#[must_use]
pub fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// Whether any pattern in `patterns` matches `name`.
#[must_use]
pub fn any_name_matches(patterns: &[String], name: &str) -> bool {
    patterns.iter().any(|p| name_matches(p, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let c = SelectionConfig::default();
        assert_eq!(c.prefer_family, FamilyPreference::Ipv4);
        assert!(!c.allow_loopback);
        assert!(!c.allow_link_local);
        assert!(!c.allow_vpn);
        assert!(!c.allow_container);
        assert!(!c.include_down);
    }

    #[test]
    fn parses_a_full_config() {
        let toml = r#"
            [selection]
            prefer_family = "ipv6"
            allow_vpn = true
            exclude_interfaces = ["docker*", "virbr0"]
            prefer_interfaces = ["eno2"]
            require_interface = "wlo1"

            [output]
            format = "json"
            default_port = 3000
        "#;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.selection.prefer_family, FamilyPreference::Ipv6);
        assert!(c.selection.allow_vpn);
        assert_eq!(c.selection.exclude_interfaces, vec!["docker*", "virbr0"]);
        assert_eq!(c.selection.require_interface.as_deref(), Some("wlo1"));
        assert_eq!(c.output.format, OutputFormat::Json);
        assert_eq!(c.output.default_port, Some(3000));
    }

    #[test]
    fn an_empty_config_is_the_default_config() {
        assert_eq!(Config::from_toml("").unwrap(), Config::default());
    }

    #[test]
    fn unknown_keys_are_rejected_rather_than_ignored() {
        // A typo that silently does nothing is worse than a loud failure.
        let err = Config::from_toml("[selection]\nprefer_familly = \"ipv4\"\n");
        assert!(err.is_err());
    }

    #[test]
    fn family_preference_parsing() {
        assert_eq!(
            "ipv4".parse::<FamilyPreference>().unwrap(),
            FamilyPreference::Ipv4
        );
        assert_eq!(
            "V6".parse::<FamilyPreference>().unwrap(),
            FamilyPreference::Ipv6
        );
        assert_eq!(
            "  any ".parse::<FamilyPreference>().unwrap(),
            FamilyPreference::Any
        );
        assert!("ipv5".parse::<FamilyPreference>().is_err());
    }

    #[test]
    fn family_preference_admits_only_its_own_family() {
        use crate::model::Family;
        assert!(FamilyPreference::Ipv4.admits(Family::V4));
        assert!(!FamilyPreference::Ipv4.admits(Family::V6));
        assert!(FamilyPreference::Any.admits(Family::V4));
        assert!(FamilyPreference::Any.admits(Family::V6));
    }

    #[test]
    fn name_matching_handles_exact_and_prefix() {
        assert!(name_matches("docker0", "docker0"));
        assert!(!name_matches("docker0", "docker1"));
        assert!(name_matches("docker*", "docker0"));
        assert!(name_matches("br-*", "br-18642d3532b2"));
        assert!(!name_matches("br-*", "br0"));
        // A bare `*` matches everything.
        assert!(name_matches("*", "wlo1"));
    }

    /// The three variables, hand-set. Never the runner's own environment: that
    /// differs per platform and would make the assertions untrustworthy.
    fn paths(xdg: Option<&str>, appdata: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
        path_from(
            xdg.map(OsString::from),
            appdata.map(OsString::from),
            home.map(OsString::from),
        )
    }

    #[test]
    fn xdg_config_home_wins_when_set() {
        assert_eq!(
            paths(Some("/cfg"), Some("/appdata"), Some("/home/u")),
            Some(PathBuf::from("/cfg/autonet/config.toml"))
        );
    }

    #[test]
    fn appdata_is_tried_after_xdg_and_before_home() {
        // The Windows case: no XDG, and `.config` must not appear — APPDATA
        // already names a configuration root.
        assert_eq!(
            paths(None, Some("/appdata"), Some("/home/u")),
            Some(PathBuf::from("/appdata/autonet/config.toml"))
        );
    }

    #[test]
    fn home_is_the_last_resort_and_grows_a_dot_config() {
        assert_eq!(
            paths(None, None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/autonet/config.toml"))
        );
    }

    #[test]
    fn an_empty_variable_falls_through_rather_than_rooting_at_nothing() {
        // `XDG_CONFIG_HOME=""` would otherwise yield `/autonet/config.toml`.
        assert_eq!(
            paths(Some(""), Some("/appdata"), Some("/home/u")),
            Some(PathBuf::from("/appdata/autonet/config.toml"))
        );
        assert_eq!(paths(Some(""), Some(""), Some("")), None);
        assert_eq!(paths(None, None, None), None);
    }
}

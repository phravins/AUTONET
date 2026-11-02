//! Find addresses written by hand into a project.
//!
//! An address typed into source code is correct until the laptop joins another
//! network, and then it is wrong for everyone who pulled it. `autonet check`
//! reports these so that they can be replaced before they are committed.
//!
//! This scanner is a line-at-a-time text search, not a parser, and it will never
//! be exact. It leans towards silence wherever a number is only shaped like an
//! address: a false alarm on every commit is what gets a hook deleted, and then
//! it catches nothing. Anything it wrongly reports can be opted out of on the
//! line itself with [`ALLOW_MARKER`].

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::classify::classify_address;
use crate::generate;
use crate::model::AddressScope;

/// A comment containing this text exempts its line, for a real fixed address
/// such as a router's.
pub const ALLOW_MARKER: &str = "autonet:allow";

/// Files larger than this are data, not something a person typed an address into.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Generated dependency manifests, whose contents nobody writes by hand.
const LOCKFILES: &[&str] = &[
    "bun.lock",
    "Cargo.lock",
    "composer.lock",
    "flake.lock",
    "Gemfile.lock",
    "go.sum",
    "gradle.lockfile",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "Pipfile.lock",
    "pnpm-lock.yaml",
    "Podfile.lock",
    "poetry.lock",
    "pubspec.lock",
    "uv.lock",
    "yarn.lock",
];

/// Prose and logs. An example address in a README cannot break a running app, and
/// blocking a commit over one would teach people to bypass the hook. A log is
/// worse: a dev server's own log names the address it was given.
const PROSE_EXTENSIONS: &[&str] = &["adoc", "log", "md", "mdx", "rst", "txt"];

/// Directories holding dependencies and build output, skipped when there is no
/// `git ls-files` to say what the project's own files are.
const SKIPPED_DIRECTORIES: &[&str] = &[
    ".angular",
    ".dart_tool",
    ".expo",
    ".git",
    ".gradle",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "Pods",
    "target",
    "vendor",
    "venv",
];

/// A hand-written address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// One-based line number.
    pub line: usize,
    /// One-based byte offset within the line.
    pub column: usize,
    /// The address found.
    pub ip: IpAddr,
}

/// Whether a directory walk should descend into a directory with this name.
pub fn is_skipped_directory(name: &str) -> bool {
    SKIPPED_DIRECTORIES.contains(&name)
}

/// Whether a file at this path is worth reading at all.
pub fn is_checked_path(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if LOCKFILES.contains(&name) {
        return false;
    }
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    !PROSE_EXTENSIONS
        .iter()
        .any(|prose| prose.eq_ignore_ascii_case(extension))
}

/// Every hand-written address in a file's text.
///
/// A file AutoNet generated is skipped, because containing an address is what it
/// is for.
pub fn scan_text(text: &str) -> Vec<Finding> {
    if generate::is_generated(text) {
        return Vec::new();
    }
    text.lines()
        .enumerate()
        .flat_map(|(index, line)| {
            scan_line(line)
                .into_iter()
                .map(move |(column, ip)| Finding {
                    line: index + 1,
                    column,
                    ip,
                })
        })
        .collect()
}

/// Every hand-written address on one line, with its one-based column.
fn scan_line(line: &str) -> Vec<(usize, IpAddr)> {
    if line.contains(ALLOW_MARKER) {
        return Vec::new();
    }
    let mut found: Vec<(usize, IpAddr)> = candidates(line, is_v4_char)
        .filter_map(|(start, token)| {
            v4_finding(line, start, token).map(|ip| (start + 1, IpAddr::V4(ip)))
        })
        .chain(candidates(line, is_v6_char).filter_map(|(start, token)| {
            v6_finding(line, start, token).map(|ip| (start + 1, IpAddr::V6(ip)))
        }))
        .collect();
    found.sort_by_key(|(column, _)| *column);
    found
}

fn is_v4_char(c: char) -> bool {
    c.is_ascii_digit() || c == '.'
}

fn is_v6_char(c: char) -> bool {
    c.is_ascii_hexdigit() || c == ':'
}

/// The maximal runs of matching characters, with their byte offsets.
fn candidates(line: &str, matches: fn(char) -> bool) -> impl Iterator<Item = (usize, &str)> {
    line.match_indices(move |c: char| !matches(c))
        .map(|(index, separator)| index + separator.len())
        .chain(std::iter::once(0))
        .filter_map(move |start| {
            let rest = &line[start..];
            let end = rest.find(|c: char| !matches(c)).unwrap_or(rest.len());
            (end > 0).then(|| (start, &rest[..end]))
        })
}

/// The characters immediately around a token, used to tell an address from part
/// of a longer word.
fn neighbours(line: &str, start: usize, token: &str) -> (Option<char>, Option<char>) {
    (
        line[..start].chars().next_back(),
        line[start + token.len()..].chars().next(),
    )
}

/// Whether a `/` and a digit follow, making this a network range and not a host
/// anyone could be connecting to.
fn is_cidr(line: &str, end: usize) -> bool {
    let mut rest = line[end..].chars();
    rest.next() == Some('/') && rest.next().is_some_and(|c| c.is_ascii_digit())
}

fn v4_finding(line: &str, start: usize, token: &str) -> Option<Ipv4Addr> {
    let trimmed = token.trim_end_matches('.');
    let ip: Ipv4Addr = trimmed.parse().ok()?;

    let (before, after) = neighbours(line, start, trimmed);
    let glued = |c: char| c.is_alphanumeric() || c == '_';
    if before.is_some_and(|c| glued(c) || c == '.') || after.is_some_and(glued) {
        return None;
    }
    if is_cidr(line, start + trimmed.len()) {
        return None;
    }
    // A four-part version number looks exactly like an address.
    if line[..start].to_ascii_lowercase().contains("version") {
        return None;
    }
    // The network and broadcast addresses of a /24, which name a range and are
    // not hosts a server could be running on.
    if matches!(ip.octets()[3], 0 | 255) {
        return None;
    }

    matches!(
        classify_address(&IpAddr::V4(ip)),
        AddressScope::Private | AddressScope::Cgnat
    )
    .then_some(ip)
}

fn v6_finding(line: &str, start: usize, token: &str) -> Option<Ipv6Addr> {
    if token.matches(':').count() < 2 {
        return None;
    }
    let ip: Ipv6Addr = token.parse().ok()?;

    let (before, after) = neighbours(line, start, token);
    let glued = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
    if before.is_some_and(glued) || after.is_some_and(glued) {
        return None;
    }
    if is_cidr(line, start + token.len()) {
        return None;
    }

    (classify_address(&IpAddr::V6(ip)) == AddressScope::UniqueLocal).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(line: &str) -> Vec<String> {
        scan_text(line).iter().map(|f| f.ip.to_string()).collect()
    }

    #[test]
    fn the_three_private_ranges_are_all_reported() {
        assert_eq!(
            ips("fetch('http://192.168.1.20:3000/api')"),
            ["192.168.1.20"]
        );
        assert_eq!(ips("HOST=10.0.0.5"), ["10.0.0.5"]);
        assert_eq!(ips("const h = \"172.20.10.4\";"), ["172.20.10.4"]);
    }

    #[test]
    fn a_carrier_grade_nat_address_is_reported() {
        assert_eq!(ips("host: 100.64.1.2"), ["100.64.1.2"]);
    }

    #[test]
    fn a_unique_local_ipv6_address_is_reported() {
        assert_eq!(ips("url = \"http://[fd00::1]:3000\""), ["fd00::1"]);
    }

    #[test]
    fn the_position_of_each_finding_is_exact() {
        let found = scan_text("first line\n  host = 10.0.0.5, 192.168.1.9\n");
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].line, found[0].column), (2, 10));
        assert_eq!((found[1].line, found[1].column), (2, 20));
    }

    #[test]
    fn addresses_that_are_the_same_everywhere_are_ignored() {
        for line in [
            "listen(3000, '0.0.0.0')",
            "http://127.0.0.1:3000",
            "dns: 8.8.8.8",
            "http://[::1]:3000",
        ] {
            assert_eq!(ips(line), Vec::<String>::new(), "{line}");
        }
    }

    #[test]
    fn documentation_ranges_are_ignored() {
        for line in ["192.0.2.1", "198.51.100.7", "203.0.113.9", "2001:db8::1"] {
            assert_eq!(ips(line), Vec::<String>::new(), "{line}");
        }
    }

    #[test]
    fn link_local_is_ignored_because_the_cloud_metadata_address_lives_there() {
        assert_eq!(ips("http://169.254.169.254/latest"), Vec::<String>::new());
    }

    #[test]
    fn a_network_range_is_not_a_host() {
        for line in [
            "allow 192.168.0.0/16",
            "subnet: 10.1.2.3/24",
            "fd00::/8",
            "mask 192.168.1.0",
            "bcast 192.168.1.255",
        ] {
            assert_eq!(ips(line), Vec::<String>::new(), "{line}");
        }
    }

    #[test]
    fn version_numbers_are_not_addresses() {
        for line in [
            "\"version\": \"10.2.3.4\"",
            "AssemblyVersion(\"10.1.1.1\")",
            "release v10.2.3.4",
            "lib_10.2.3.4",
            "1.10.0.0.1",
        ] {
            assert_eq!(ips(line), Vec::<String>::new(), "{line}");
        }
    }

    #[test]
    fn numbers_too_large_for_an_address_are_ignored() {
        assert_eq!(ips("10.0.0.256"), Vec::<String>::new());
        assert_eq!(ips("192.168.001.002"), Vec::<String>::new());
    }

    #[test]
    fn a_rust_style_path_is_not_an_ipv6_address() {
        assert_eq!(ips("use fd00::thing;"), Vec::<String>::new());
    }

    #[test]
    fn a_line_can_opt_out_for_a_real_fixed_address() {
        assert_eq!(
            ips("const ROUTER = '192.168.1.1'; // autonet:allow"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_opt_out_covers_only_its_own_line() {
        let found = scan_text("a = '10.0.0.5' # autonet:allow\nb = '10.0.0.6'\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn a_generated_file_is_skipped_whole() {
        let text = format!("# {}\nAUTONET_IP=192.168.1.20\n", generate::MARKER);
        assert!(scan_text(&text).is_empty());
    }

    #[test]
    fn multibyte_text_around_an_address_does_not_break_the_scan() {
        assert_eq!(ips("→ サーバー 192.168.1.20 ←"), ["192.168.1.20"]);
    }

    #[test]
    fn lockfiles_and_prose_are_not_read() {
        for path in [
            "package-lock.json",
            "app/yarn.lock",
            "README.md",
            "docs\\notes.TXT",
        ] {
            assert!(!is_checked_path(path), "{path}");
        }
        for path in ["src/api.ts", ".env", "Makefile", "lib/main.dart"] {
            assert!(is_checked_path(path), "{path}");
        }
    }

    #[test]
    fn dependency_directories_are_skipped_in_a_walk() {
        assert!(is_skipped_directory("node_modules"));
        assert!(!is_skipped_directory("src"));
    }
}

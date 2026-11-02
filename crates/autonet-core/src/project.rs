//! Decide how to run a project in development, from facts about its files.
//!
//! Pure: the CLI reads the disk and hands over [`ProjectFacts`]; this module says
//! what kind of project that is, what to run, and which address file it needs.
//! Every row of the README's project table is a fixture test here.
//!
//! It **never guesses**. A dev server started with the wrong flags still starts,
//! and then answers on the wrong port or only on localhost while AutoNet prints a
//! URL that does not work. So a `dev` script that is not recognisably a plain
//! invocation of the framework is refused, with the `autonet.toml` that would
//! make it explicit, rather than having flags appended and hoped about.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use crate::generate::{Format, OutputSpec};
use crate::model::AddressScope;

/// The project file `autonet init` writes, committed and containing no address.
pub const MANIFEST_NAME: &str = "autonet.toml";

/// What `autonet.toml` declares. Anything left out falls back to detection.
///
/// A separate schema from the user-level `config.toml`: that file describes a
/// person's machine and this one describes a repository. Unknown keys are refused
/// for the same reason, so that a typo fails instead of doing nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Manifest {
    /// The command to run, as an argument vector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// The port the URL is built for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Commands to run alongside `command`, such as a local API for a mobile app.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<Background>,
    /// The files to keep current.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<OutputSpec>,
}

/// A command run alongside the main one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Background {
    /// The command to run, as an argument vector.
    pub command: Vec<String>,
}

impl Manifest {
    /// Parse and validate the text of an `autonet.toml`.
    ///
    /// # Errors
    ///
    /// Returns a message for a parse failure, an empty command, a zero port, a
    /// prefix on a format that has none, or an output path outside the project.
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), String> {
        if self.command.as_ref().is_some_and(Vec::is_empty) {
            return Err("`command` is empty".into());
        }
        if self.background.iter().any(|b| b.command.is_empty()) {
            return Err("a [[background]] `command` is empty".into());
        }
        if self.port == Some(0) {
            return Err("`port` cannot be 0".into());
        }
        for output in &self.output {
            // The one category of project file AutoNet writes to is the paths
            // named here, so a path that leaves the project would turn a checked-
            // in manifest into a way to overwrite files elsewhere on the machine.
            let path = Path::new(&output.path);
            let inside = !output.path.is_empty()
                && path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_) | Component::CurDir));
            if !inside {
                return Err(format!(
                    "output path `{}` must be relative and stay inside the project",
                    output.path
                ));
            }
            if output.prefix.is_some() && output.format != Format::Env {
                return Err(format!(
                    "output `{}`: only the env format has a `prefix`",
                    output.path
                ));
            }
        }
        Ok(())
    }
}

/// How a project installs and runs its scripts, read from its lockfile.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PackageManager {
    /// npm, the default when there is no other lockfile.
    #[default]
    Npm,
    /// pnpm.
    Pnpm,
    /// Yarn.
    Yarn,
    /// Bun.
    Bun,
}

impl PackageManager {
    fn program(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }

    /// The argument vector that runs `script` with extra flags.
    ///
    /// npm is the one that needs `--`. The others pass flags straight through,
    /// and some versions of them hand a literal `--` to the script.
    fn run(self, script: &str, flags: Vec<String>) -> Vec<String> {
        let mut command = vec![self.program().to_string(), "run".into(), script.into()];
        if self == Self::Npm && !flags.is_empty() {
            command.push("--".into());
        }
        command.extend(flags);
        command
    }
}

/// What `package.json` says, reduced to what detection needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageFacts {
    /// The `scripts` table.
    pub scripts: BTreeMap<String, String>,
    /// Names from `dependencies` and `devDependencies` together.
    pub dependencies: BTreeSet<String>,
}

/// Said when a project gets a generated file with no URL in it.
///
/// A mobile app and the local API it calls is the usual pairing, and the app has
/// no port of its own to detect. Without this, `AutoNet.url` is simply null.
pub const NO_PORT_NOTE: &str =
    "No port is set, so the generated url is empty. If this app calls a local API, add `port = 3000` to autonet.toml, with the API's port.";

/// What the CLI found on disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProjectFacts {
    /// A parsed `package.json`.
    pub package: Option<PackageFacts>,
    /// From the lockfile present.
    pub package_manager: PackageManager,
    /// `next.config` already says who may load dev resources.
    pub next_dev_origins: bool,
    /// `src/env` already declares the generated URL to `SvelteKit`.
    pub sveltekit_env_declared: bool,
    /// `tsconfig.json` exists, so a generated `.ts` module can be imported.
    pub typescript: bool,
    /// `pubspec.yaml` exists.
    pub pubspec: bool,
    /// `src-tauri/` exists.
    pub src_tauri: bool,
    /// `manage.py` exists.
    pub manage_py: bool,
    /// `build.gradle` or `build.gradle.kts` exists at the root.
    pub gradle: bool,
    /// A `*.xcodeproj` exists at the root.
    pub xcodeproj: bool,
    /// The Python found on `PATH`, for `manage.py`.
    pub python: Option<String>,
    /// Whether the debug Android manifest allows plain `http://`, if there is one.
    pub android_cleartext: Option<bool>,
    /// An `ios/` directory or a root Xcode project exists.
    pub ios: bool,
}

/// The kinds of project AutoNet recognises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Kind {
    Vite,
    SvelteKit,
    Astro,
    Nuxt,
    Next,
    Angular,
    CreateReactApp,
    Electron,
    Tauri,
    Expo,
    ReactNative,
    Flutter,
    Android,
    Ios,
    NativeMobile,
    Django,
}

impl DevPlan {
    /// Whether this plan starts anything that could listen on a port.
    pub fn runs_something(&self) -> bool {
        self.command.is_some() || !self.background.is_empty()
    }

    /// Whether this plan may start on an address of this scope.
    ///
    /// On a public address a wildcard bind is the internet, not the LAN. A dev
    /// server has no authentication and often exposes source maps and an
    /// evaluation endpoint, so that takes a flag naming the risk. Keeping a file
    /// current exposes nothing, so a plan that starts nothing is never stopped.
    pub fn may_start_on(&self, scope: AddressScope, allow_public: bool) -> bool {
        allow_public || scope != AddressScope::Global || !self.runs_something()
    }
}

impl Kind {
    /// The name shown to the developer.
    pub fn label(self) -> &'static str {
        match self {
            Self::Vite => "Vite",
            Self::SvelteKit => "SvelteKit",
            Self::Astro => "Astro",
            Self::Nuxt => "Nuxt",
            Self::Next => "Next.js",
            Self::Angular => "Angular",
            Self::CreateReactApp => "Create React App",
            Self::Electron => "Electron",
            Self::Tauri => "Tauri",
            Self::Expo => "Expo",
            Self::ReactNative => "React Native",
            Self::Flutter => "Flutter",
            Self::Android => "Android",
            Self::Ios => "iOS",
            Self::NativeMobile => "Android and iOS",
            Self::Django => "Django",
        }
    }

    /// What the developer must do after a network change, in their own terms.
    pub fn action(self) -> &'static str {
        match self {
            Self::Vite | Self::SvelteKit | Self::Astro | Self::Nuxt | Self::Next => {
                "The dev server reloads the new values. Open the new URL on your device."
            }
            Self::Angular | Self::CreateReactApp => {
                "The file watcher rebuilds. Open the new URL on your device."
            }
            Self::Electron => "Reload the window, or re-read autonet.generated.json.",
            Self::Tauri => {
                "Desktop keeps working. For Tauri mobile, restart `autonet dev`: TAURI_DEV_HOST is read at launch."
            }
            Self::Expo | Self::ReactNative => {
                "Restart `autonet dev`: Metro advertises the address it was started with, so the device needs the new one."
            }
            Self::Flutter => "Press r in this terminal to hot reload.",
            Self::Android | Self::Ios | Self::NativeMobile => {
                "Rebuild the app: these values are compiled in."
            }
            Self::Django => "The server keeps answering on the new address. Nothing to do.",
        }
    }

    /// How source code reads the address, shown beside a hardcoded one.
    pub fn usage(self) -> &'static str {
        match self {
            Self::Vite => "import.meta.env.VITE_AUTONET_URL",
            Self::SvelteKit => {
                "PUBLIC_AUTONET_URL, from $app/env/public (SvelteKit 2: $env/static/public)"
            }
            Self::Astro => "import.meta.env.PUBLIC_AUTONET_URL",
            Self::Next => "process.env.NEXT_PUBLIC_AUTONET_URL",
            Self::Nuxt | Self::Angular | Self::CreateReactApp | Self::Expo | Self::ReactNative => {
                "AutoNet.url, imported from the generated module"
            }
            Self::Electron | Self::Tauri => "the `url` field of autonet.generated.json",
            Self::Flutter => "AutoNet.url, from lib/autonet.g.dart",
            Self::Android => "the autonet.url property in autonet.properties",
            Self::Ios => "AUTONET_URL from Autonet.xcconfig",
            Self::NativeMobile => {
                "autonet.url in autonet.properties, or AUTONET_URL in Autonet.xcconfig"
            }
            Self::Django => "the AUTONET_URL environment variable",
        }
    }
}

/// The value of an environment variable that depends on the selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvValue {
    /// The selected address.
    Ip,
    /// The wildcard address the server binds.
    Bind,
    /// The port.
    Port,
}

/// How `autonet dev` will run a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevPlan {
    /// What was detected. `None` when `autonet.toml` declared everything.
    pub kind: Option<Kind>,
    /// What to run in the foreground. `None` when the project only needs its
    /// files kept current, as a natively built mobile app does.
    pub command: Option<Vec<String>>,
    /// Commands run alongside it.
    pub background: Vec<Vec<String>>,
    /// The port the URL is built for.
    pub port: Option<u16>,
    /// Whether AutoNet told the dev server to listen on every interface.
    pub binds_wildcard: bool,
    /// Variables set for the command beyond the `AUTONET_*` three.
    pub env: Vec<(String, EnvValue)>,
    /// The files to keep current.
    pub outputs: Vec<OutputSpec>,
}

/// Why no plan could be made. The text is what the developer should read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

/// The `autonet.toml` shown whenever detection stops short.
const MANIFEST_EXAMPLE: &str = r#"Declare it in autonet.toml:

    command = ["npm", "run", "dev"]   # must listen on 0.0.0.0, and on this port
    port = 3000

    [[output]]
    format = "env"                    # or ts, js, json, dart, properties, xcconfig
    path = ".env.autonet""#;

/// What kind of project these files describe, if any.
pub fn detect(facts: &ProjectFacts) -> Option<Kind> {
    if facts.pubspec {
        return Some(Kind::Flutter);
    }

    if let Some(package) = &facts.package {
        let has = |name: &str| package.dependencies.contains(name);
        let from_package = if has("expo") {
            Some(Kind::Expo)
        } else if has("react-native") {
            Some(Kind::ReactNative)
        } else if facts.src_tauri || has("@tauri-apps/cli") {
            Some(Kind::Tauri)
        } else if has("electron") {
            Some(Kind::Electron)
        } else if has("next") {
            Some(Kind::Next)
        } else if has("nuxt") {
            Some(Kind::Nuxt)
        } else if has("astro") {
            Some(Kind::Astro)
        } else if has("@sveltejs/kit") {
            Some(Kind::SvelteKit)
        } else if has("@angular/core") {
            Some(Kind::Angular)
        } else if has("react-scripts") {
            Some(Kind::CreateReactApp)
        } else if has("vite") {
            Some(Kind::Vite)
        } else {
            None
        };
        if from_package.is_some() {
            return from_package;
        }
    }

    if facts.src_tauri {
        Some(Kind::Tauri)
    } else if facts.manage_py {
        Some(Kind::Django)
    } else if facts.gradle && facts.xcodeproj {
        Some(Kind::NativeMobile)
    } else if facts.gradle {
        Some(Kind::Android)
    } else if facts.xcodeproj {
        Some(Kind::Ios)
    } else {
        None
    }
}

/// The address files a kind of project reads.
fn default_outputs(kind: Kind, facts: &ProjectFacts) -> Vec<OutputSpec> {
    // Every framework here loads this name in development and ranks it above the
    // others, and unlike `.env.local` it is rarely a file someone already keeps
    // secrets in.
    let env = |prefix: &str| OutputSpec {
        format: Format::Env,
        path: ".env.development.local".into(),
        prefix: Some(prefix.into()),
    };
    let file = |format, path: &str| OutputSpec {
        format,
        path: path.into(),
        prefix: None,
    };
    let script_module = |stem: &str| {
        if facts.typescript {
            file(Format::Ts, &format!("{stem}.ts"))
        } else {
            file(Format::Js, &format!("{stem}.js"))
        }
    };

    match kind {
        Kind::Vite => vec![env("VITE_")],
        Kind::SvelteKit | Kind::Astro => vec![env("PUBLIC_")],
        Kind::Next => vec![env("NEXT_PUBLIC_")],
        // A module and not an env file. Nuxt reads only `.env` unless `--dotenv`
        // names another, and naming another stops it reading the project's own.
        Kind::Nuxt => vec![file(Format::Ts, "autonet.generated.ts")],
        Kind::Angular => vec![file(Format::Ts, "src/autonet.generated.ts")],
        Kind::CreateReactApp => vec![script_module("src/autonet.generated")],
        Kind::Expo | Kind::ReactNative => vec![script_module("autonet.generated")],
        Kind::Electron | Kind::Tauri => vec![file(Format::Json, "autonet.generated.json")],
        Kind::Flutter => vec![file(Format::Dart, "lib/autonet.g.dart")],
        Kind::Android => vec![file(Format::Properties, "autonet.properties")],
        Kind::Ios => vec![file(Format::Xcconfig, "Autonet.xcconfig")],
        Kind::NativeMobile => vec![
            file(Format::Properties, "autonet.properties"),
            file(Format::Xcconfig, "Autonet.xcconfig"),
        ],
        Kind::Django => vec![file(Format::Env, ".env.autonet")],
    }
}

/// The files a project's `init` should set up, without needing a runnable plan.
pub fn outputs_for(facts: &ProjectFacts, manifest: Option<&Manifest>) -> Vec<OutputSpec> {
    match manifest.filter(|manifest| !manifest.output.is_empty()) {
        Some(manifest) => manifest.output.clone(),
        None => detect(facts).map_or_else(Vec::new, |kind| default_outputs(kind, facts)),
    }
}

/// A `scripts` entry, split into what it would run.
struct Script {
    name: String,
    tokens: Vec<String>,
}

impl Script {
    /// The value following a flag, and whether the flag is present at all.
    ///
    /// Two different answers that matter: `--host` alone means every interface,
    /// and no `--host` means loopback.
    #[allow(clippy::option_option)]
    fn flag(&self, names: &[&str]) -> Option<Option<&str>> {
        for (index, token) in self.tokens.iter().enumerate() {
            if let Some((name, value)) = token.split_once('=') {
                if names.contains(&name) {
                    return Some(Some(value));
                }
            }
            if names.contains(&token.as_str()) {
                let value = self
                    .tokens
                    .get(index + 1)
                    .map(String::as_str)
                    .filter(|value| !value.starts_with('-'));
                return Some(value);
            }
        }
        None
    }
}

/// Split a script and drop any leading `NAME=value` and `cross-env`.
///
/// Returns `None` for anything using shell syntax, because then it is more than
/// one program and flags appended to it reach only the last.
fn tokenize(script: &str) -> Option<Vec<String>> {
    if script.contains(['&', '|', ';', '>', '<', '$', '`', '(', '\'', '"']) {
        return None;
    }
    let tokens: Vec<&str> = script
        .split_whitespace()
        .skip_while(|token| *token == "cross-env" || is_assignment(token))
        .collect();
    (!tokens.is_empty()).then(|| tokens.into_iter().map(String::from).collect())
}

fn is_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The first of the usual scripts that plainly runs one of `programs`.
///
/// Each pattern is the program and, if it has one, its subcommand.
fn find_script(package: &PackageFacts, programs: &[&[&str]]) -> Option<Script> {
    ["dev", "start", "serve"].into_iter().find_map(|name| {
        let tokens = tokenize(package.scripts.get(name)?)?;
        programs
            .iter()
            .any(|pattern| matches_program(&tokens, pattern))
            .then(|| Script {
                name: name.into(),
                tokens,
            })
    })
}

fn matches_program(tokens: &[String], pattern: &[&str]) -> bool {
    if tokens.len() < pattern.len() || pattern.iter().zip(tokens).any(|(want, got)| want != got) {
        return false;
    }
    // A bare `vite` is the dev server, but `vite build` is not.
    match tokens.get(pattern.len()) {
        Some(next) if pattern.len() == 1 => next.starts_with('-'),
        _ => true,
    }
}

/// How one framework is told where to listen.
struct Recipe {
    programs: &'static [&'static [&'static str]],
    default_port: u16,
    port_flags: &'static [&'static str],
    host_flags: &'static [&'static str],
    flags: fn(bind: &str, port: Option<u16>) -> Vec<String>,
}

fn pair(flag: &str, value: &dyn ToString) -> [String; 2] {
    [flag.to_string(), value.to_string()]
}

fn host_port(host_flag: &str, port_flag: &str, bind: &str, port: Option<u16>) -> Vec<String> {
    let mut flags = pair(host_flag, &bind).to_vec();
    if let Some(port) = port {
        flags.extend(pair(port_flag, &port));
    }
    flags
}

const VITE_PROGRAMS: &[&[&str]] = &[&["vite", "dev"], &["vite", "serve"], &["vite"]];

/// `--strictPort` because Vite otherwise moves to the next free port without
/// being asked, and then the printed URL and the generated file are both wrong.
fn vite_flags(bind: &str, port: Option<u16>) -> Vec<String> {
    let mut flags = host_port("--host", "--port", bind, port);
    flags.push("--strictPort".into());
    flags
}

fn recipe(kind: Kind) -> Option<Recipe> {
    const DOUBLE_DASH_PORT: &[&str] = &["--port"];
    const HOST: &[&str] = &["--host"];
    Some(match kind {
        Kind::Vite | Kind::SvelteKit => Recipe {
            programs: VITE_PROGRAMS,
            default_port: 5173,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: HOST,
            flags: vite_flags,
        },
        Kind::Astro => Recipe {
            programs: &[&["astro", "dev"]],
            default_port: 4321,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: HOST,
            flags: |bind, port| host_port("--host", "--port", bind, port),
        },
        Kind::Nuxt => Recipe {
            programs: &[&["nuxt", "dev"], &["nuxi", "dev"]],
            default_port: 3000,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: HOST,
            flags: |bind, port| host_port("--host", "--port", bind, port),
        },
        Kind::Next => Recipe {
            programs: &[&["next", "dev"]],
            default_port: 3000,
            port_flags: &["-p", "--port"],
            host_flags: &["-H", "--hostname"],
            flags: |bind, port| host_port("-H", "-p", bind, port),
        },
        Kind::Angular => Recipe {
            programs: &[&["ng", "serve"]],
            default_port: 4200,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: HOST,
            flags: |bind, port| host_port("--host", "--port", bind, port),
        },
        Kind::CreateReactApp => Recipe {
            programs: &[&["react-scripts", "start"]],
            default_port: 3000,
            port_flags: &[],
            host_flags: &[],
            // Told through HOST and PORT in the environment: see `env_for`.
            flags: |_, _| Vec::new(),
        },
        Kind::Expo => Recipe {
            programs: &[&["expo", "start"]],
            default_port: 8081,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: &["--lan", "--host"],
            flags: |_, port| {
                let mut flags = vec!["--lan".to_string()];
                if let Some(port) = port {
                    flags.extend(pair("--port", &port));
                }
                flags
            },
        },
        Kind::ReactNative => Recipe {
            programs: &[&["react-native", "start"]],
            default_port: 8081,
            port_flags: DOUBLE_DASH_PORT,
            host_flags: HOST,
            flags: |bind, port| host_port("--host", "--port", bind, port),
        },
        _ => return None,
    })
}

/// A host value in a script that listens on every interface.
fn is_wildcard(value: Option<&str>) -> bool {
    matches!(
        value,
        None | Some("0.0.0.0" | "::" | "[::]" | "true" | "lan")
    )
}

fn env_for(kind: Kind) -> Vec<(String, EnvValue)> {
    let var = |name: &str, value| (name.to_string(), value);
    match kind {
        Kind::CreateReactApp => vec![var("HOST", EnvValue::Bind), var("PORT", EnvValue::Port)],
        Kind::Expo | Kind::ReactNative => {
            vec![var("REACT_NATIVE_PACKAGER_HOSTNAME", EnvValue::Ip)]
        }
        Kind::Tauri => vec![var("TAURI_DEV_HOST", EnvValue::Ip)],
        _ => Vec::new(),
    }
}

/// The command and port for a framework run through a `package.json` script.
fn scripted_command(
    kind: Kind,
    recipe: &Recipe,
    facts: &ProjectFacts,
    requested_port: Option<u16>,
    bind: &str,
) -> Result<(Vec<String>, u16), Refusal> {
    let package = facts.package.as_ref().ok_or_else(|| {
        Refusal(format!(
            "{} needs a package.json to run from.\n\n{MANIFEST_EXAMPLE}",
            kind.label()
        ))
    })?;

    let Some(script) = find_script(package, recipe.programs) else {
        return Err(Refusal(format!(
            "this looks like a {} project, but none of its dev, start or serve scripts is a plain `{}`, so AutoNet cannot tell what it would listen on.\n\n{MANIFEST_EXAMPLE}",
            kind.label(),
            recipe.programs[0].join(" "),
        )));
    };

    let script_port = script
        .flag(recipe.port_flags)
        .flatten()
        .and_then(|port| port.parse::<u16>().ok());
    if let (Some(fixed), Some(requested)) = (script_port, requested_port) {
        if fixed != requested {
            return Err(Refusal(format!(
                "the `{}` script fixes port {fixed}, but port {requested} was asked for. Change one of them.",
                script.name
            )));
        }
    }
    let port = requested_port
        .or(script_port)
        .unwrap_or(recipe.default_port);

    if let Some(host) = script.flag(recipe.host_flags) {
        if !is_wildcard(host) {
            return Err(Refusal(format!(
                "the `{}` script binds {}, which other devices cannot reach. Remove that flag, and AutoNet will bind every interface for you.",
                script.name,
                host.unwrap_or("a specific address"),
            )));
        }
    }

    // Only the flags the script does not already give: a parser that sees one
    // twice may use either, and a script's own `--port` is the one that is right.
    let mut flags = (recipe.flags)(bind, (script_port.is_none()).then_some(port));
    if script.flag(recipe.host_flags).is_some() {
        flags = strip_flag(flags, recipe.host_flags);
    }

    Ok((facts.package_manager.run(&script.name, flags), port))
}

/// Remove a flag and its value from a list of generated flags.
fn strip_flag(flags: Vec<String>, names: &[&str]) -> Vec<String> {
    let mut kept = Vec::with_capacity(flags.len());
    let mut iterator = flags.into_iter().peekable();
    while let Some(flag) = iterator.next() {
        if names.contains(&flag.as_str()) {
            if iterator.peek().is_some_and(|next| !next.starts_with('-')) {
                iterator.next();
            }
        } else {
            kept.push(flag);
        }
    }
    kept
}

/// The command for a kind that has no framework flags to set.
fn unflagged_command(kind: Kind, facts: &ProjectFacts) -> Result<Option<Vec<String>>, Refusal> {
    let not_runnable = |what: String| Err(Refusal(format!("{what}\n\n{MANIFEST_EXAMPLE}")));
    match kind {
        Kind::Flutter => Ok(Some(vec!["flutter".into(), "run".into()])),
        // Built by the IDE. The file is what they need, and there is nothing
        // sensible to launch from a terminal.
        Kind::Android | Kind::Ios | Kind::NativeMobile => Ok(None),
        Kind::Electron => {
            let package = facts.package.as_ref();
            match ["dev", "start"]
                .into_iter()
                .find(|name| package.is_some_and(|p| p.scripts.contains_key(*name)))
            {
                Some(name) => Ok(Some(facts.package_manager.run(name, Vec::new()))),
                None => not_runnable("this Electron project has no dev or start script.".into()),
            }
        }
        Kind::Tauri => {
            let has_script = facts
                .package
                .as_ref()
                .is_some_and(|p| p.scripts.contains_key("tauri"));
            if has_script {
                Ok(Some(facts.package_manager.run("tauri", vec!["dev".into()])))
            } else {
                Ok(Some(vec!["cargo".into(), "tauri".into(), "dev".into()]))
            }
        }
        Kind::Django => match &facts.python {
            Some(python) => Ok(Some(vec![
                python.clone(),
                "manage.py".into(),
                "runserver".into(),
            ])),
            None => not_runnable("no python3 or python was found on PATH.".into()),
        },
        _ => unreachable!("framework kinds have a recipe"),
    }
}

/// Decide how to run a project.
///
/// `requested_port` is the `--port` flag; `bind` is the wildcard address matching
/// the selected family, so that the plan is the same on every machine.
///
/// # Errors
///
/// Returns a [`Refusal`] when the project is not recognised, or when what it
/// declares cannot be run faithfully.
pub fn plan(
    facts: &ProjectFacts,
    manifest: Option<&Manifest>,
    requested_port: Option<u16>,
    bind: &str,
) -> Result<DevPlan, Refusal> {
    let kind = detect(facts);
    let declared = manifest.cloned().unwrap_or_default();
    let port_wanted = requested_port.or(declared.port);

    let (command, port, binds_wildcard) = match (&declared.command, kind) {
        // Literal. Declared means the author has taken responsibility for it, so
        // nothing is appended and AutoNet does not claim to know what it binds.
        (Some(command), _) => (Some(command.clone()), port_wanted, false),

        (None, Some(kind)) => {
            if let Some(recipe) = recipe(kind) {
                let (command, port) = scripted_command(kind, &recipe, facts, port_wanted, bind)?;
                (Some(command), Some(port), true)
            } else if kind == Kind::Django {
                let port = port_wanted.unwrap_or(8000);
                let mut command = unflagged_command(kind, facts)?.unwrap_or_default();
                command.push(django_address(bind, port));
                (Some(command), Some(port), true)
            } else {
                (unflagged_command(kind, facts)?, port_wanted, false)
            }
        }

        (None, None) => {
            // Outputs alone are enough: the file is kept current and nothing runs.
            if declared.output.is_empty() {
                return Err(Refusal(format!(
                    "AutoNet does not recognise this project.\n\n{MANIFEST_EXAMPLE}"
                )));
            }
            (None, port_wanted, false)
        }
    };

    let outputs = if declared.output.is_empty() {
        match kind {
            Some(kind) => default_outputs(kind, facts),
            None => vec![OutputSpec {
                format: Format::Env,
                path: ".env.autonet".into(),
                prefix: None,
            }],
        }
    } else {
        declared.output.clone()
    };

    Ok(DevPlan {
        kind,
        command,
        background: declared
            .background
            .iter()
            .map(|b| b.command.clone())
            .collect(),
        port,
        binds_wildcard,
        env: kind.map_or_else(Vec::new, env_for),
        outputs,
    })
}

fn django_address(bind: &str, port: u16) -> String {
    if bind.contains(':') {
        format!("[{bind}]:{port}")
    } else {
        format!("{bind}:{port}")
    }
}

/// Whether an Android manifest has dealt with plain `http://` traffic.
///
/// A network security config counts, because it is the more careful way to allow
/// it and AutoNet cannot tell from here what that file permits.
pub fn manifest_allows_cleartext(text: &str) -> bool {
    text.contains("usesCleartextTraffic=\"true\"") || text.contains("networkSecurityConfig")
}

/// What the developer needs to know before this project can work from a phone or
/// another machine, which AutoNet will not change for them.
pub fn notes(kind: Option<Kind>, facts: &ProjectFacts) -> Vec<String> {
    let mut notes = Vec::new();

    if facts.android_cleartext == Some(false) {
        notes.push(
            "Android blocks plain http:// by default. For debug builds only, add android:usesCleartextTraffic=\"true\" to <application> in src/debug/AndroidManifest.xml."
                .to_string(),
        );
    }
    if facts.ios
        && matches!(
            kind,
            Some(Kind::Flutter | Kind::Ios | Kind::NativeMobile | Kind::Expo | Kind::ReactNative)
        )
    {
        notes.push(
            "iOS blocks plain http:// by default. For debug builds only, set NSAppTransportSecurity > NSAllowsLocalNetworking in Info.plist."
                .to_string(),
        );
    }
    match kind {
        Some(Kind::Django) => notes.push(
            "Django answers 400 for hosts it does not know. For development only, set ALLOWED_HOSTS = [\"*\"] when DEBUG is on."
                .to_string(),
        ),
        // Verified against Next 16: without this it answers other devices 403 for
        // dev assets and refuses the hot reload socket. Patterns and not the
        // current address, because the config is only read at startup and the
        // address is exactly what is going to change.
        Some(Kind::Next) if !facts.next_dev_origins => notes.push(
            "Next.js 16 refuses dev assets and hot reload to other devices. Add this to next.config: allowedDevOrigins: ['192.168.*.*', '10.*.*.*', '172.*.*.*']"
                .to_string(),
        ),
        // Verified against Kit 3.1: an undeclared variable is not an error, the
        // module just has no such export and the page renders an empty value. So
        // nothing would ever point at this if AutoNet did not.
        Some(Kind::SvelteKit) if !facts.sveltekit_env_declared => notes.push(
            "SvelteKit 3 hides undeclared variables, and the value comes out empty. In src/env.js or src/env.ts add: export const variables = defineEnvVars({ PUBLIC_AUTONET_URL: { public: true } }). SvelteKit 2 needs nothing."
                .to_string(),
        ),
        _ => {}
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dependency: &str, script: &str) -> ProjectFacts {
        ProjectFacts {
            package: Some(PackageFacts {
                scripts: BTreeMap::from([("dev".to_string(), script.to_string())]),
                dependencies: BTreeSet::from([dependency.to_string()]),
            }),
            ..ProjectFacts::default()
        }
    }

    fn planned(facts: &ProjectFacts) -> DevPlan {
        plan(facts, None, None, "0.0.0.0").expect("a plan")
    }

    fn command(plan: &DevPlan) -> String {
        plan.command.as_ref().expect("a command").join(" ")
    }

    fn output(plan: &DevPlan) -> (&Format, &str, Option<&str>) {
        let first = &plan.outputs[0];
        (&first.format, first.path.as_str(), first.prefix.as_deref())
    }

    #[test]
    fn vite_is_told_where_to_listen_and_not_to_wander_off_that_port() {
        let plan = planned(&package("vite", "vite"));
        assert_eq!(plan.kind, Some(Kind::Vite));
        assert_eq!(
            command(&plan),
            "npm run dev -- --host 0.0.0.0 --port 5173 --strictPort"
        );
        assert_eq!(plan.port, Some(5173));
        assert!(plan.binds_wildcard);
        assert_eq!(
            output(&plan),
            (&Format::Env, ".env.development.local", Some("VITE_"))
        );
    }

    #[test]
    fn next_uses_its_own_flag_spellings_and_prefix() {
        let plan = planned(&package("next", "next dev --turbopack"));
        assert_eq!(command(&plan), "npm run dev -- -H 0.0.0.0 -p 3000");
        assert_eq!(output(&plan).2, Some("NEXT_PUBLIC_"));
    }

    #[test]
    fn nuxt_gets_a_module_so_that_it_keeps_reading_its_own_env_file() {
        let plan = planned(&package("nuxt", "nuxt dev"));
        assert_eq!(command(&plan), "npm run dev -- --host 0.0.0.0 --port 3000");
        assert_eq!(output(&plan), (&Format::Ts, "autonet.generated.ts", None));
    }

    #[test]
    fn astro_and_sveltekit_use_the_public_prefix() {
        let astro = planned(&package("astro", "astro dev"));
        assert_eq!(command(&astro), "npm run dev -- --host 0.0.0.0 --port 4321");
        assert_eq!(output(&astro).2, Some("PUBLIC_"));

        let kit = planned(&package("@sveltejs/kit", "vite dev"));
        assert_eq!(kit.kind, Some(Kind::SvelteKit));
        assert_eq!(output(&kit).2, Some("PUBLIC_"));
    }

    #[test]
    fn sveltekit_is_recognised_even_though_vite_is_also_a_dependency() {
        let mut facts = package("@sveltejs/kit", "vite dev");
        facts
            .package
            .as_mut()
            .unwrap()
            .dependencies
            .insert("vite".into());
        assert_eq!(detect(&facts), Some(Kind::SvelteKit));
    }

    #[test]
    fn angular_writes_a_module_because_it_has_no_dev_env_file() {
        let mut facts = package("@angular/core", "");
        let package = facts.package.as_mut().unwrap();
        package.scripts.clear();
        package.scripts.insert("start".into(), "ng serve".into());
        let plan = planned(&facts);
        assert_eq!(
            command(&plan),
            "npm run start -- --host 0.0.0.0 --port 4200"
        );
        assert_eq!(
            output(&plan),
            (&Format::Ts, "src/autonet.generated.ts", None)
        );
    }

    #[test]
    fn create_react_app_is_configured_through_the_environment() {
        let mut facts = package("react-scripts", "");
        let package = facts.package.as_mut().unwrap();
        package.scripts.clear();
        package
            .scripts
            .insert("start".into(), "react-scripts start".into());
        let plan = planned(&facts);
        assert_eq!(command(&plan), "npm run start");
        assert_eq!(plan.env[0], ("HOST".to_string(), EnvValue::Bind));
        assert_eq!(plan.env[1], ("PORT".to_string(), EnvValue::Port));
    }

    #[test]
    fn a_generated_module_is_javascript_unless_the_project_uses_typescript() {
        let mut facts = package("react-scripts", "react-scripts start");
        assert_eq!(
            output(&planned(&facts)),
            (&Format::Js, "src/autonet.generated.js", None)
        );
        facts.typescript = true;
        assert_eq!(
            output(&planned(&facts)),
            (&Format::Ts, "src/autonet.generated.ts", None)
        );
    }

    #[test]
    fn electron_runs_its_own_script_and_gets_a_json_file() {
        let plan = planned(&package("electron", "electron ."));
        assert_eq!(command(&plan), "npm run dev");
        assert!(
            !plan.binds_wildcard,
            "nothing about Electron was told to listen"
        );
        assert_eq!(
            output(&plan),
            (&Format::Json, "autonet.generated.json", None)
        );
    }

    #[test]
    fn tauri_is_detected_from_its_directory_and_gets_the_dev_host_variable() {
        let facts = ProjectFacts {
            src_tauri: true,
            ..ProjectFacts::default()
        };
        let plan = planned(&facts);
        assert_eq!(plan.kind, Some(Kind::Tauri));
        assert_eq!(command(&plan), "cargo tauri dev");
        assert_eq!(plan.env, [("TAURI_DEV_HOST".to_string(), EnvValue::Ip)]);
    }

    #[test]
    fn tauri_with_a_frontend_goes_through_its_script() {
        let mut facts = package("@tauri-apps/cli", "vite");
        facts.src_tauri = true;
        facts
            .package
            .as_mut()
            .unwrap()
            .scripts
            .insert("tauri".into(), "tauri".into());
        assert_eq!(command(&planned(&facts)), "npm run tauri -- dev");
    }

    #[test]
    fn expo_and_react_native_advertise_the_selected_address_to_the_device() {
        let expo = planned(&package("expo", "expo start"));
        assert_eq!(command(&expo), "npm run dev -- --lan --port 8081");
        assert_eq!(
            expo.env,
            [("REACT_NATIVE_PACKAGER_HOSTNAME".to_string(), EnvValue::Ip)]
        );
        assert_eq!(output(&expo).1, "autonet.generated.js");

        let native = planned(&package("react-native", "react-native start"));
        assert_eq!(native.kind, Some(Kind::ReactNative));
        assert_eq!(
            command(&native),
            "npm run dev -- --host 0.0.0.0 --port 8081"
        );
    }

    #[test]
    fn flutter_runs_in_the_terminal_with_no_port_of_its_own() {
        let facts = ProjectFacts {
            pubspec: true,
            ..ProjectFacts::default()
        };
        let plan = planned(&facts);
        assert_eq!(command(&plan), "flutter run");
        assert_eq!(plan.port, None);
        assert_eq!(output(&plan), (&Format::Dart, "lib/autonet.g.dart", None));
    }

    #[test]
    fn a_flutter_project_with_an_api_takes_its_port_from_the_manifest() {
        let facts = ProjectFacts {
            pubspec: true,
            ..ProjectFacts::default()
        };
        let manifest =
            Manifest::parse("port = 3000\n[[background]]\ncommand = [\"node\", \"api.js\"]\n")
                .unwrap();
        let plan = plan(&facts, Some(&manifest), None, "0.0.0.0").unwrap();
        assert_eq!(plan.port, Some(3000));
        assert_eq!(
            plan.background,
            [vec!["node".to_string(), "api.js".to_string()]]
        );
        assert_eq!(command(&plan), "flutter run");
    }

    #[test]
    fn native_mobile_projects_only_need_their_files_kept_current() {
        let android = planned(&ProjectFacts {
            gradle: true,
            ..ProjectFacts::default()
        });
        assert_eq!(android.command, None);
        assert_eq!(output(&android).1, "autonet.properties");

        let ios = planned(&ProjectFacts {
            xcodeproj: true,
            ..ProjectFacts::default()
        });
        assert_eq!(output(&ios).1, "Autonet.xcconfig");

        let both = planned(&ProjectFacts {
            gradle: true,
            xcodeproj: true,
            ..ProjectFacts::default()
        });
        assert_eq!(both.outputs.len(), 2);
    }

    #[test]
    fn django_listens_on_the_wildcard_through_runserver() {
        let facts = ProjectFacts {
            manage_py: true,
            python: Some("python3".into()),
            ..ProjectFacts::default()
        };
        let plan = planned(&facts);
        assert_eq!(command(&plan), "python3 manage.py runserver 0.0.0.0:8000");
        assert_eq!(output(&plan), (&Format::Env, ".env.autonet", None));
    }

    #[test]
    fn the_ipv6_wildcard_is_bracketed_for_runserver() {
        let facts = ProjectFacts {
            manage_py: true,
            python: Some("python3".into()),
            ..ProjectFacts::default()
        };
        let plan = plan(&facts, None, None, "::").unwrap();
        assert_eq!(command(&plan), "python3 manage.py runserver [::]:8000");
    }

    #[test]
    fn django_without_a_python_is_refused_rather_than_failing_to_launch() {
        let facts = ProjectFacts {
            manage_py: true,
            ..ProjectFacts::default()
        };
        assert!(plan(&facts, None, None, "0.0.0.0").is_err());
    }

    #[test]
    fn other_package_managers_do_not_get_nmps_separator() {
        for (manager, expected) in [
            (PackageManager::Pnpm, "pnpm run dev --host"),
            (PackageManager::Yarn, "yarn run dev --host"),
            (PackageManager::Bun, "bun run dev --host"),
        ] {
            let mut facts = package("vite", "vite");
            facts.package_manager = manager;
            assert!(
                command(&planned(&facts)).starts_with(expected),
                "{manager:?}"
            );
        }
    }

    #[test]
    fn a_port_in_the_script_is_used_and_not_given_twice() {
        let plan = planned(&package("vite", "vite --port 4000"));
        assert_eq!(plan.port, Some(4000));
        assert_eq!(command(&plan), "npm run dev -- --host 0.0.0.0 --strictPort");
    }

    #[test]
    fn a_requested_port_overrides_the_framework_default() {
        let plan = plan(&package("vite", "vite"), None, Some(9000), "0.0.0.0").unwrap();
        assert_eq!(plan.port, Some(9000));
        assert!(command(&plan).contains("--port 9000"));
    }

    #[test]
    fn a_requested_port_that_contradicts_the_script_is_refused() {
        let refused = plan(
            &package("vite", "vite --port 4000"),
            None,
            Some(9000),
            "0.0.0.0",
        );
        assert!(refused.unwrap_err().0.contains("fixes port 4000"));
    }

    #[test]
    fn a_script_that_already_binds_the_wildcard_is_left_to_it() {
        let plan = planned(&package("vite", "vite --host"));
        assert_eq!(command(&plan), "npm run dev -- --port 5173 --strictPort");
        assert!(plan.binds_wildcard);
    }

    #[test]
    fn a_script_that_binds_localhost_is_refused_rather_than_printing_a_dead_url() {
        let refused = plan(
            &package("vite", "vite --host localhost"),
            None,
            None,
            "0.0.0.0",
        );
        assert!(refused.unwrap_err().0.contains("binds localhost"));
    }

    #[test]
    fn a_script_with_shell_syntax_is_refused_with_the_manifest_to_write() {
        for script in ["vite && echo done", "concurrently vite api", "vite build"] {
            let refused = plan(&package("vite", script), None, None, "0.0.0.0");
            let message = refused.unwrap_err().0;
            assert!(message.contains("autonet.toml"), "{script}: {message}");
        }
    }

    #[test]
    fn leading_environment_assignments_do_not_hide_the_program() {
        let plan = planned(&package("vite", "cross-env NODE_ENV=development vite"));
        assert_eq!(plan.kind, Some(Kind::Vite));
    }

    #[test]
    fn an_unrecognised_project_is_refused() {
        let refused = plan(&ProjectFacts::default(), None, None, "0.0.0.0");
        assert!(refused.unwrap_err().0.contains("does not recognise"));
    }

    #[test]
    fn a_declared_command_is_run_literally() {
        let manifest = Manifest::parse(
            "command = [\"uvicorn\", \"main:app\", \"--host\", \"0.0.0.0\"]\nport = 8000\n",
        )
        .unwrap();
        let plan = plan(&ProjectFacts::default(), Some(&manifest), None, "0.0.0.0").unwrap();
        assert_eq!(command(&plan), "uvicorn main:app --host 0.0.0.0");
        assert_eq!(plan.port, Some(8000));
        assert!(!plan.binds_wildcard, "AutoNet cannot know what it binds");
        assert_eq!(output(&plan).1, ".env.autonet");
    }

    #[test]
    fn declared_outputs_replace_the_detected_ones() {
        let manifest =
            Manifest::parse("[[output]]\nformat = \"json\"\npath = \"public/host.json\"\n")
                .unwrap();
        let plan = plan(&package("vite", "vite"), Some(&manifest), None, "0.0.0.0").unwrap();
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(output(&plan).1, "public/host.json");
        assert!(
            command(&plan).contains("--strictPort"),
            "the command is still detected"
        );
    }

    #[test]
    fn a_manifest_with_only_outputs_runs_nothing() {
        let manifest =
            Manifest::parse("[[output]]\nformat = \"json\"\npath = \"host.json\"\n").unwrap();
        let plan = plan(&ProjectFacts::default(), Some(&manifest), None, "0.0.0.0").unwrap();
        assert_eq!(plan.command, None);
    }

    #[test]
    fn a_manifest_may_not_write_outside_the_project() {
        for path in ["../elsewhere", "/etc/hosts", "a/../../b", ""] {
            let text = format!("[[output]]\nformat = \"env\"\npath = \"{path}\"\n");
            assert!(Manifest::parse(&text).is_err(), "{path}");
        }
        assert!(Manifest::parse("[[output]]\nformat = \"env\"\npath = \"./a/b.env\"\n").is_ok());
    }

    #[test]
    fn a_manifest_with_a_typo_or_nonsense_is_refused() {
        for text in [
            "comand = [\"x\"]",
            "command = []",
            "port = 0",
            "[[background]]\ncommand = []",
            "[[output]]\nformat = \"ts\"\npath = \"a\"\nprefix = \"VITE_\"",
        ] {
            assert!(Manifest::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn next_is_told_about_dev_origins_until_its_config_has_them() {
        let facts = package("next", "next dev");
        assert!(notes(detect(&facts), &facts)
            .iter()
            .any(|note| note.contains("allowedDevOrigins")));

        let configured = ProjectFacts {
            next_dev_origins: true,
            ..facts
        };
        assert_eq!(
            notes(detect(&configured), &configured),
            Vec::<String>::new()
        );
    }

    #[test]
    fn sveltekit_is_told_to_declare_the_variable_until_it_has() {
        let facts = package("@sveltejs/kit", "vite dev");
        assert!(notes(detect(&facts), &facts)
            .iter()
            .any(|note| note.contains("defineEnvVars")));

        let declared = ProjectFacts {
            sveltekit_env_declared: true,
            ..facts
        };
        assert_eq!(notes(detect(&declared), &declared), Vec::<String>::new());
    }

    #[test]
    fn a_public_address_stops_a_plan_that_starts_something_unless_allowed() {
        let plan = planned(&package("vite", "vite"));
        assert!(!plan.may_start_on(AddressScope::Global, false));
        assert!(plan.may_start_on(AddressScope::Global, true));
        assert!(plan.may_start_on(AddressScope::Private, false));
    }

    #[test]
    fn a_public_address_does_not_stop_a_plan_that_only_writes_a_file() {
        let plan = planned(&ProjectFacts {
            gradle: true,
            ..ProjectFacts::default()
        });
        assert!(plan.may_start_on(AddressScope::Global, false));
    }

    #[test]
    fn background_commands_count_as_starting_something() {
        let manifest =
            Manifest::parse("[[background]]\ncommand = [\"node\", \"api.js\"]\n[[output]]\nformat = \"env\"\npath = \"a\"\n")
                .unwrap();
        let plan = plan(&ProjectFacts::default(), Some(&manifest), None, "0.0.0.0").unwrap();
        assert!(!plan.may_start_on(AddressScope::Global, false));
    }

    #[test]
    fn cleartext_is_recognised_either_way_it_can_be_allowed() {
        assert!(manifest_allows_cleartext(
            "<application android:usesCleartextTraffic=\"true\">"
        ));
        assert!(manifest_allows_cleartext(
            "<application android:networkSecurityConfig=\"@xml/n\">"
        ));
        assert!(!manifest_allows_cleartext(
            "<application android:usesCleartextTraffic=\"false\">"
        ));
        assert!(!manifest_allows_cleartext("<application>"));
    }

    #[test]
    fn notes_cover_the_blocks_autonet_will_not_change_for_you() {
        let flutter = ProjectFacts {
            pubspec: true,
            android_cleartext: Some(false),
            ios: true,
            ..ProjectFacts::default()
        };
        let notes = notes(detect(&flutter), &flutter);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("usesCleartextTraffic"));
        assert!(notes[1].contains("NSAllowsLocalNetworking"));
    }

    #[test]
    fn no_note_is_given_when_cleartext_is_already_allowed() {
        let flutter = ProjectFacts {
            pubspec: true,
            android_cleartext: Some(true),
            ..ProjectFacts::default()
        };
        assert_eq!(notes(detect(&flutter), &flutter), Vec::<String>::new());
    }
}

//! Read the project in a directory into the facts `autonet_core::project` decides from.
//!
//! The disk half of detection, and it decides nothing. Everything about what the
//! facts mean is in the core crate, where it can be tested without a filesystem.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use autonet_core::project::{
    manifest_allows_cleartext, Manifest, PackageFacts, PackageManager, ProjectFacts, MANIFEST_NAME,
};
use serde::Deserialize;

use crate::CliError;

/// Where an Android manifest can be, with the debug ones first because a debug
/// build merges them over the main one.
const ANDROID_MANIFESTS: &[&str] = &[
    "android/app/src/debug/AndroidManifest.xml",
    "app/src/debug/AndroidManifest.xml",
    "src/debug/AndroidManifest.xml",
    "android/app/src/main/AndroidManifest.xml",
    "app/src/main/AndroidManifest.xml",
    "src/main/AndroidManifest.xml",
];

/// Every name Next.js reads its config from.
const NEXT_CONFIGS: &[&str] = &[
    "next.config.js",
    "next.config.mjs",
    "next.config.ts",
    "next.config.cjs",
];

/// Where `SvelteKit` 3 reads its declared environment variables from.
const SVELTEKIT_ENV_FILES: &[&str] = &["src/env.ts", "src/env.js"];

/// The parts of `package.json` that detection looks at.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PackageJson {
    scripts: BTreeMap<String, String>,
    dependencies: BTreeMap<String, serde_json::Value>,
    dev_dependencies: BTreeMap<String, serde_json::Value>,
}

/// Read the facts about the project at `root`.
///
/// # Errors
///
/// Returns [`CliError::Usage`] if `package.json` exists and cannot be read. That
/// is reported rather than treated as absent, or a stray comma would come out as
/// "AutoNet does not recognise this project".
pub(crate) fn read_facts(root: &Path) -> Result<ProjectFacts, CliError> {
    let is_file = |name: &str| root.join(name).is_file();
    let xcodeproj = has_xcode_project(root);
    let manage_py = is_file("manage.py");

    Ok(ProjectFacts {
        package: read_package(root)?,
        package_manager: package_manager(root),
        next_dev_origins: NEXT_CONFIGS.iter().any(|name| {
            fs::read_to_string(root.join(name)).is_ok_and(|text| text.contains("allowedDevOrigins"))
        }),
        sveltekit_env_declared: SVELTEKIT_ENV_FILES.iter().any(|name| {
            fs::read_to_string(root.join(name))
                .is_ok_and(|text| text.contains("PUBLIC_AUTONET_URL"))
        }),
        typescript: is_file("tsconfig.json"),
        pubspec: is_file("pubspec.yaml"),
        src_tauri: root.join("src-tauri").is_dir(),
        manage_py,
        gradle: is_file("build.gradle") || is_file("build.gradle.kts"),
        xcodeproj,
        python: manage_py.then(python).flatten(),
        android_cleartext: android_cleartext(root),
        ios: xcodeproj || root.join("ios").is_dir(),
    })
}

/// Read `autonet.toml`, if there is one.
///
/// # Errors
///
/// Returns [`CliError::Usage`] naming the file if it cannot be read or is wrong.
pub(crate) fn read_manifest(root: &Path) -> Result<Option<Manifest>, CliError> {
    let path = root.join(MANIFEST_NAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CliError::Usage(format!(
                "cannot read {MANIFEST_NAME}: {error}"
            )))
        }
    };
    Manifest::parse(&text)
        .map(Some)
        .map_err(|message| CliError::Usage(format!("{MANIFEST_NAME}: {message}")))
}

fn read_package(root: &Path) -> Result<Option<PackageFacts>, CliError> {
    let text = match fs::read_to_string(root.join("package.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(CliError::Usage(format!(
                "cannot read package.json: {error}"
            )))
        }
    };
    let parsed: PackageJson = serde_json::from_str(&text)
        .map_err(|error| CliError::Usage(format!("cannot parse package.json: {error}")))?;

    let dependencies: BTreeSet<String> = parsed
        .dependencies
        .into_keys()
        .chain(parsed.dev_dependencies.into_keys())
        .collect();
    Ok(Some(PackageFacts {
        scripts: parsed.scripts,
        dependencies,
    }))
}

/// Which package manager the lockfile belongs to, the same evidence they use.
fn package_manager(root: &Path) -> PackageManager {
    let has = |name: &str| root.join(name).is_file();
    if has("pnpm-lock.yaml") {
        PackageManager::Pnpm
    } else if has("yarn.lock") {
        PackageManager::Yarn
    } else if has("bun.lock") || has("bun.lockb") {
        PackageManager::Bun
    } else {
        PackageManager::Npm
    }
}

fn has_xcode_project(root: &Path) -> bool {
    fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "xcodeproj")
        })
    })
}

/// The program name, not the path it was found at, so the command AutoNet prints
/// is the one a person would type. An activated virtualenv is first on `PATH`.
fn python() -> Option<String> {
    ["python3", "python", "py"]
        .into_iter()
        .find(|name| autonet_platform::find_program(name).is_some())
        .map(String::from)
}

/// Whether any Android manifest allows plain `http://`, if there is a manifest.
fn android_cleartext(root: &Path) -> Option<bool> {
    let texts: Vec<String> = ANDROID_MANIFESTS
        .iter()
        .filter_map(|path| fs::read_to_string(root.join(path)).ok())
        .collect();
    (!texts.is_empty()).then(|| texts.iter().any(|text| manifest_allows_cleartext(text)))
}

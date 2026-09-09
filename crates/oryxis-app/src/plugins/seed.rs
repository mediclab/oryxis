//! Import the offline bundle's plugin seeds into the cache.
//!
//! The bundle (`crate::bundle`) ships each plugin as the pair the
//! catalog publishes it as: `plugins/<provider>.json`, the catalog
//! entry the workflow pulled the binary by, and `plugins/<binary>`
//! beside it. The embedded copy is a SEED, never a third resolution
//! path: it is written into the ordinary version cache through the
//! same gates a download passes (SHA-256 against the catalog entry,
//! Ed25519 against the baked-in anchors, atomic write, `.sig` sidecar),
//! and from then on the host spawns it, the panel lists it, the updater
//! replaces it and the retention prune ages it out exactly as it would
//! a downloaded version. An embedded plugin with a path of its own
//! would age inside the installer with no way out.
//!
//! Runs once per boot, before the plugin rows are built, and is cheap
//! when there is nothing to do: one catalog read per provider and a
//! `stat` of the version directory; the hash and the signature are
//! only paid for a version the cache does not hold yet. Activation is
//! the one decision that is not "import": a seed becomes `current`
//! when nothing is, or when it is newer than what is and the user has
//! not pinned a version, and never otherwise, so a bundle installed
//! over a machine that already updated past it changes nothing.

use std::path::{Path, PathBuf};

use super::manifest::{version_key, PluginManifest};
use super::{cache, download, PluginError};

/// What one seed did, for the boot log and the MCP launcher refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Imported {
    pub provider_id: String,
    pub version: String,
    /// The seed became the active version (`current` now names it).
    pub activated: bool,
}

/// Import every seed found next to the executable. `pinned` answers
/// the user's pinned version for a provider, read from the settings
/// table by the caller so this module never touches the vault.
pub(crate) fn import_bundled(pinned: &dyn Fn(&str) -> Option<String>) -> Vec<Imported> {
    let Some(dir) = crate::bundle::plugins_dir() else {
        return Vec::new();
    };
    if !dir.is_dir() {
        return Vec::new();
    }
    import_from(
        &dir,
        env!("CARGO_PKG_VERSION"),
        oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS,
        pinned,
    )
}

/// Import every `<provider>.json` + binary pair under `dir`. A seed
/// that fails is logged and skipped so one bad file cannot take the
/// others down; a seed this app build cannot run (too old an app, a
/// protocol it does not speak, no binary for this platform) is skipped
/// quietly, since the ordinary download path would refuse it for the
/// same reasons.
pub(crate) fn import_from(
    dir: &Path,
    app_version: &str,
    protocols: &[u32],
    pinned: &dyn Fn(&str) -> Option<String>,
) -> Vec<Imported> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut catalogs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    catalogs.sort();
    for catalog in catalogs {
        match import_one(&catalog, app_version, protocols, pinned) {
            Ok(Some(imported)) => {
                tracing::info!(
                    target = "oryxis::plugins",
                    provider = %imported.provider_id,
                    version = %imported.version,
                    activated = imported.activated,
                    "plugin seed imported from the offline bundle"
                );
                out.push(imported);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                target = "oryxis::plugins",
                catalog = %catalog.display(),
                error = %e,
                "plugin seed skipped"
            ),
        }
    }
    out
}

fn import_one(
    catalog: &Path,
    app_version: &str,
    protocols: &[u32],
    pinned: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Imported>, PluginError> {
    let stem = catalog
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| PluginError::Manifest("catalog file has no name".into()))?;
    let body = std::fs::read_to_string(catalog)?;
    let manifest = PluginManifest::parse(&body)?;
    // The same self-identification the catalog fetch demands: a file
    // serving another provider means the layout drifted, and importing
    // it would seed the wrong plugin under this name.
    if manifest.provider_id != stem {
        return Err(PluginError::Manifest(format!(
            "seed catalog {stem}.json declares provider_id {}",
            manifest.provider_id
        )));
    }
    let id = manifest.provider_id.as_str();
    let Some(entry) = manifest.best(app_version, protocols) else {
        return Ok(None);
    };
    let Some(binary) = entry.binary_for_current_platform() else {
        return Ok(None);
    };
    let version = entry.version.as_str();
    let held = cache::binary_path(id, version)?.exists();
    if !held {
        let src = catalog.with_file_name(cache::binary_name(id));
        if !src.is_file() {
            return Err(PluginError::BinaryNotFound(src));
        }
        let bytes = std::fs::read(&src)?;
        download::install_verified(id, version, binary, bytes)?;
    }
    let current = cache::current_version(id)?;
    let activate = activation(version, current.as_deref(), pinned(id).as_deref());
    if activate {
        cache::set_current(id, version)?;
    }
    if held && !activate {
        return Ok(None);
    }
    Ok(Some(Imported {
        provider_id: id.to_string(),
        version: version.to_string(),
        activated: activate,
    }))
}

/// Whether a seed at `seed` becomes the active version, given what
/// `current` names today and the version the user pinned, if any. Pure,
/// so the rule is testable apart from the disk: nothing active means
/// yes; a pin means only the pinned version may move `current`; else a
/// strictly newer seed wins and an equal or older one changes nothing.
pub(crate) fn activation(seed: &str, current: Option<&str>, pinned: Option<&str>) -> bool {
    match (current, pinned) {
        (None, _) => true,
        (Some(cur), Some(pin)) => pin == seed && cur != seed,
        (Some(cur), None) => version_key(seed) > version_key(cur),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::verify::DEV_SEED;
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest, Sha256};

    #[test]
    fn activation_rule() {
        assert!(activation("1.2.0", None, None));
        assert!(activation("1.2.0", None, Some("1.0.0")));
        assert!(activation("1.2.0", Some("1.1.9"), None));
        assert!(!activation("1.2.0", Some("1.2.0"), None));
        assert!(!activation("1.2.0", Some("1.3.0"), None));
        // A pin holds `current` still for anything but the pin itself.
        assert!(!activation("1.2.0", Some("1.1.0"), Some("1.1.0")));
        assert!(activation("1.1.0", Some("1.0.0"), Some("1.1.0")));
        assert!(!activation("1.1.0", Some("1.1.0"), Some("1.1.0")));
    }

    /// Write a seed dir holding one provider signed with the dev key
    /// (trusted by the debug build's `verify`), returning the dir.
    fn seed_dir(provider: &str, version: &str, bytes: &[u8], sha_ok: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let sk = SigningKey::from_bytes(&DEV_SEED);
        let sig = STANDARD.encode(sk.sign(bytes).to_bytes());
        let mut sha: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        if !sha_ok {
            sha = sha.chars().rev().collect();
        }
        let manifest = serde_json::json!({
            "provider_id": provider,
            "versions": [{
                "version": version,
                "protocol_versions": oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS,
                "min_app": "0.1.0",
                "binaries": [{
                    "os": crate::plugins::manifest::current_os(),
                    "arch": crate::plugins::manifest::current_arch(),
                    "url": "https://example.invalid/never-dialled",
                    "sha256": sha,
                    "signature": sig,
                    "size": bytes.len(),
                }],
            }],
        });
        std::fs::write(dir.path().join(format!("{provider}.json")), manifest.to_string()).unwrap();
        std::fs::write(dir.path().join(cache::binary_name(provider)), bytes).unwrap();
        dir
    }

    /// The dev anchor only exists in debug builds; a release-profile
    /// test run has no key that could accept the seed.
    fn signing_available() -> bool {
        cfg!(debug_assertions)
    }

    #[test]
    fn seed_lands_in_the_cache_and_activates_once() {
        if !signing_available() {
            return;
        }
        let seeds = seed_dir("aws", "0.9.0", b"aws plugin bytes", true);
        let cache_root = tempfile::tempdir().unwrap();
        cache::with_test_root(cache_root.path(), || {
            let none = |_: &str| None;
            let first = import_from(seeds.path(), "1.0.0", oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS, &none);
            assert_eq!(
                first,
                vec![Imported { provider_id: "aws".into(), version: "0.9.0".into(), activated: true }]
            );
            let bin = cache::current_binary("aws").unwrap().expect("seed is the active binary");
            assert_eq!(std::fs::read(&bin).unwrap(), b"aws plugin bytes");
            assert!(bin.with_extension("sig").exists(), "the .sig sidecar rides along");
            // A second boot finds the version held and current: nothing
            // to report, nothing re-hashed.
            let second = import_from(seeds.path(), "1.0.0", oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS, &none);
            assert!(second.is_empty());
        });
    }

    #[test]
    fn seed_never_downgrades_or_moves_a_pin() {
        if !signing_available() {
            return;
        }
        let seeds = seed_dir("aws", "0.9.0", b"aws plugin bytes", true);
        let cache_root = tempfile::tempdir().unwrap();
        cache::with_test_root(cache_root.path(), || {
            // The machine already runs a newer download.
            let newer = cache::binary_path("aws", "1.0.0").unwrap();
            std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
            std::fs::write(&newer, b"newer").unwrap();
            cache::set_current("aws", "1.0.0").unwrap();
            let none = |_: &str| None;
            let got = import_from(seeds.path(), "1.0.0", oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS, &none);
            assert_eq!(got.len(), 1, "the seed is still imported as a version");
            assert!(!got[0].activated);
            assert_eq!(cache::current_version("aws").unwrap().as_deref(), Some("1.0.0"));
        });
        // An older active version pinned by the user stays.
        let cache_root = tempfile::tempdir().unwrap();
        cache::with_test_root(cache_root.path(), || {
            let older = cache::binary_path("aws", "0.5.0").unwrap();
            std::fs::create_dir_all(older.parent().unwrap()).unwrap();
            std::fs::write(&older, b"older").unwrap();
            cache::set_current("aws", "0.5.0").unwrap();
            let pinned = |_: &str| Some("0.5.0".to_string());
            let got = import_from(seeds.path(), "1.0.0", oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS, &pinned);
            assert_eq!(got.len(), 1);
            assert!(!got[0].activated);
            assert_eq!(cache::current_version("aws").unwrap().as_deref(), Some("0.5.0"));
        });
    }

    #[test]
    fn seed_with_a_wrong_hash_is_refused() {
        if !signing_available() {
            return;
        }
        let seeds = seed_dir("aws", "0.9.0", b"aws plugin bytes", false);
        let cache_root = tempfile::tempdir().unwrap();
        cache::with_test_root(cache_root.path(), || {
            let none = |_: &str| None;
            let got = import_from(seeds.path(), "1.0.0", oryxis_plugin_protocol::SUPPORTED_PROTOCOL_VERSIONS, &none);
            assert!(got.is_empty());
            assert!(cache::current_binary("aws").unwrap().is_none());
            assert!(cache::installed_versions("aws").unwrap().is_empty());
        });
    }

    #[test]
    fn seed_the_app_cannot_run_is_skipped() {
        if !signing_available() {
            return;
        }
        let seeds = seed_dir("aws", "0.9.0", b"aws plugin bytes", true);
        let cache_root = tempfile::tempdir().unwrap();
        cache::with_test_root(cache_root.path(), || {
            let none = |_: &str| None;
            // The manifest asks for app 0.1.0; a protocol the app does
            // not speak leaves `best` empty.
            let got = import_from(seeds.path(), "1.0.0", &[u32::MAX], &none);
            assert!(got.is_empty());
            assert!(cache::installed_versions("aws").unwrap().is_empty());
        });
    }
}

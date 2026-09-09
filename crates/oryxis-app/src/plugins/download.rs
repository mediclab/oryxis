//! Fetch a plugin manifest and download + install a plugin binary.
//!
//! The install path is gated twice before a binary is made
//! reachable: the SHA-256 from the manifest must match the bytes on
//! the wire, *and* the Ed25519 signature must validate against a
//! baked-in trust anchor (see [`super::verify`]). Only then are the
//! bytes written, and the write is atomic, into a sibling `.tmp`
//! file that's renamed into place, so a half-finished download is
//! never visible as an installed version.

use std::path::PathBuf;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use super::manifest::{self, ManifestEntry, PlatformBinary, PluginManifest};
use super::{cache, verify, PluginError, RELEASE_REPO};

/// Ceiling for the releases listing body.
///
/// This one is NOT a "a few KB of metadata" payload: the response
/// carries every asset of every release in the window, so it grows
/// with the project's own release history, ~59 KB per app release and
/// ~37 KB per plugin release at the time of writing. The original
/// 1 MB ceiling was sized from a guess of ~150 KB and the real body
/// crossed it in August 2026, which broke plugin installs outright
/// (discussion #163: the modal blamed an unreachable host on a
/// network where GitHub answered fine). A ceiling this one only
/// exists to stop a hostile mirror from streaming an endless body
/// into memory, so it is set far above any plausible listing instead
/// of near it: 32 MB is ~500 more app releases of headroom and still
/// a transient allocation smaller than one plugin binary.
const MAX_RELEASES_JSON: u64 = 32 * 1024 * 1024;

/// Ceiling for a manifest asset. A real manifest is a few hundred
/// bytes and its size does not track the release history, so this one
/// stays tight.
const MAX_MANIFEST_JSON: u64 = 1024 * 1024;

/// The catalog file for a provider, tracked in this repo under
/// `plugins/` and published to the asset host by `publish-mirror.yml`.
///
/// A FIXED address is the whole point: `net_mirror::candidates` turns
/// it into "git first, asset host second" with no API call in the
/// path, and the file is the same `<provider>.json` the release
/// workflow already builds and signs off on.
fn catalog_url(provider_id: &str) -> String {
    format!("https://raw.githubusercontent.com/{RELEASE_REPO}/main/plugins/{provider_id}.json")
}

/// The manifest for a provider: the catalog file first, the release
/// API only if that fails.
///
/// Discovery used to be API-only, and it cost a MEGABYTE per call: the
/// listing carries every asset of every release in a 30-entry window,
/// of which the app read three fields. That body outgrew its own read
/// ceiling in August 2026 and took every plugin install down with it
/// (discussion #163), it re-downloaded on every boot for each
/// auto-updating plugin, and its 30-entry window was going to drop
/// `aws-v0.1.0` off the bottom within a few app releases. The catalog
/// file answers the same question in ~2 KB from a fixed URL, so none
/// of those three failure modes has anywhere to live.
///
/// The API path stays as the last resort. It covers the window
/// between a plugin release publishing its binaries and the catalog
/// commit landing, and it means a broken catalog degrades to the old
/// behaviour instead of to no plugins at all.
pub async fn fetch_manifest(provider_id: &str) -> Result<PluginManifest, PluginError> {
    // Offline mode (`crate::offline`) is answered here, once, for every
    // caller: the boot auto-update, the MCP migration, the panel's
    // check and the install modal all read the same refusal.
    if crate::offline::is_on() {
        return Err(PluginError::Offline);
    }
    let client = manifest_client()?;
    match fetch_catalog(&client, provider_id).await {
        Ok(manifest) => Ok(manifest),
        Err(catalog_err) => {
            tracing::debug!(
                target = "oryxis::plugins",
                provider = %provider_id,
                error = %catalog_err,
                "plugin catalog unavailable, falling back to the release API"
            );
            fetch_manifest_via_releases(&client, provider_id).await
        }
    }
}

fn manifest_client() -> Result<reqwest::Client, PluginError> {
    reqwest::Client::builder()
        .user_agent(concat!("Oryxis/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .https_only(true)
        .build()
        .map_err(|e| PluginError::Download(e.to_string()))
}

/// One GET of a fixed URL. `candidates` supplies the git leg and the
/// asset-host leg, so a blocked `raw.githubusercontent.com` (the
/// hardest-blocked GitHub host on mainland-China networks) resolves
/// through the mirror without a second code path here.
async fn fetch_catalog(
    client: &reqwest::Client,
    provider_id: &str,
) -> Result<PluginManifest, PluginError> {
    let body = get_github_capped(client, &catalog_url(provider_id), MAX_MANIFEST_JSON)
        .await
        .map_err(|e| PluginError::Manifest(format!("plugin catalog for {provider_id}: {e}")))?;
    let body = std::str::from_utf8(&body)
        .map_err(|e| PluginError::Download(format!("catalog not utf-8: {e}")))?;
    let manifest = PluginManifest::parse(body)?;
    // A catalog file serving the wrong provider means the layout drifted
    // (a bad copy, a mis-keyed bucket object). Refusing here sends the
    // caller to the API fallback instead of installing the wrong plugin.
    if manifest.provider_id != provider_id {
        return Err(PluginError::Manifest(format!(
            "catalog for {provider_id} declares provider_id {}",
            manifest.provider_id
        )));
    }
    Ok(manifest)
}

/// Legacy discovery: find the latest `<provider>-v*` release on GitHub
/// and download the `<provider>.json` manifest from its assets.
///
/// The plugin release workflow uploads both the binaries and a
/// matching `aws.json` (or whatever the provider is) to the same
/// GitHub Release, so the release IS a manifest source. Kept as the
/// fallback behind [`fetch_catalog`], never as the first choice: see
/// that function's note for what this path costs.
async fn fetch_manifest_via_releases(
    client: &reqwest::Client,
    provider_id: &str,
) -> Result<PluginManifest, PluginError> {
    // Step 1: list releases. 30 entries covers years of plugin
    // releases without paginating.
    let releases_url =
        format!("https://api.github.com/repos/{RELEASE_REPO}/releases?per_page=30");
    let releases_bytes = get_github_capped(client, &releases_url, MAX_RELEASES_JSON)
        .await
        .map_err(|e| {
            PluginError::Manifest(format!("github releases api for {RELEASE_REPO}: {e}"))
        })?;
    let releases: Vec<serde_json::Value> = serde_json::from_slice(&releases_bytes)
        .map_err(|e| PluginError::Download(format!("parse releases json: {e}")))?;

    // Step 2: filter by `<provider>-v` tag, require a manifest asset,
    // pick the highest version.
    let tag_prefix = format!("{provider_id}-v");
    let manifest_asset = format!("{provider_id}.json");
    let mut candidates: Vec<(&serde_json::Value, [u32; 4])> = releases
        .iter()
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let version = tag.strip_prefix(&tag_prefix)?;
            // Skip releases that don't carry the manifest asset.
            let has_manifest = r
                .get("assets")
                .and_then(|a| a.as_array())
                .map(|assets| {
                    assets.iter().any(|asset| {
                        asset.get("name").and_then(|n| n.as_str())
                            == Some(manifest_asset.as_str())
                    })
                })
                .unwrap_or(false);
            has_manifest.then(|| (r, manifest::version_key(version)))
        })
        .collect();
    candidates.sort_by_key(|(_, key)| std::cmp::Reverse(*key));
    let (release, _) = candidates.first().ok_or_else(|| {
        PluginError::Manifest(format!(
            "no `{tag_prefix}*` release with a `{manifest_asset}` asset found in {RELEASE_REPO}"
        ))
    })?;

    // Step 3: download the manifest asset itself.
    let download_url = release
        .get("assets")
        .and_then(|a| a.as_array())
        .and_then(|assets| {
            assets.iter().find(|asset| {
                asset.get("name").and_then(|n| n.as_str())
                    == Some(manifest_asset.as_str())
            })
        })
        .and_then(|asset| asset.get("browser_download_url"))
        .and_then(|u| u.as_str())
        .ok_or_else(|| {
            PluginError::Manifest("asset url missing on release payload".into())
        })?;

    let body = get_github_capped(client, download_url, MAX_MANIFEST_JSON)
        .await
        .map_err(PluginError::Download)?;
    let body = std::str::from_utf8(&body)
        .map_err(|e| PluginError::Download(format!("manifest not utf-8: {e}")))?;
    PluginManifest::parse(body)
}

/// Mirror-aware GET for GitHub-bound URLs: try each candidate
/// (configured mirror first, direct as the per-request fallback, see
/// `crate::net_mirror`) until one answers with a success status. The
/// SHA-256 + Ed25519 gates downstream keep any mirror untrusted.
async fn get_github(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, String> {
    let mut last = String::new();
    for candidate in crate::net_mirror::candidates(url) {
        match client.get(&candidate).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => last = format!("HTTP {}", resp.status()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// `get_github` plus the body read, so a candidate that answers 200
/// with an unusable body still falls through to the next one.
///
/// Reading outside the candidate loop was its own failure mode: a
/// captive portal answering 200 with an HTML block page, or a body
/// over the ceiling, aborted the whole fetch while the mirror leg sat
/// there untried. `update.rs::fetch_release` already reads inside its
/// loop for exactly this reason; this is the same contract.
async fn get_github_capped(
    client: &reqwest::Client,
    url: &str,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let mut last = String::new();
    for candidate in crate::net_mirror::candidates(url) {
        match client.get(&candidate).send().await {
            Ok(resp) if resp.status().is_success() => {
                match read_capped(resp, max_bytes).await {
                    Ok(body) => return Ok(body),
                    Err(e) => last = e,
                }
            }
            Ok(resp) => last = format!("HTTP {}", resp.status()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Read a `reqwest::Response` body into a `Vec<u8>` up to `max_bytes`.
/// Returns an error if the body exceeds the cap, either by
/// `Content-Length` advertisement or by mid-stream chunk accumulation.
/// Used by `fetch_manifest` to keep small JSON parses bounded; the
/// binary download path has its own cap because it streams to a buffer
/// the verifier needs whole.
async fn read_capped(
    resp: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    if let Some(len) = resp.content_length()
        && len > max_bytes
    {
        return Err(format!(
            "advertised {len} bytes exceeds {max_bytes} byte ceiling"
        ));
    }
    let mut buf = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if (buf.len() as u64).saturating_add(chunk.len() as u64) > max_bytes {
            return Err(format!("exceeded {max_bytes} byte ceiling mid-stream"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Download the binary for `entry` on the current platform, verify
/// it, and install it into the version cache. Returns the absolute
/// path to the installed binary.
///
/// `progress` is called as bytes arrive with `(downloaded, total)`;
/// `total` is `0` when the server doesn't send a `Content-Length`.
/// Installing does *not* flip the `current` pointer, the caller
/// decides when a freshly installed version becomes active.
pub async fn download_and_install(
    provider_id: &str,
    entry: &ManifestEntry,
    mut progress: impl FnMut(u64, u64),
) -> Result<PathBuf, PluginError> {
    // A manifest fetched before the switch flipped must not turn into a
    // download after it: the install modal keeps its size line, the
    // Install click reports the mode.
    if crate::offline::is_on() {
        return Err(PluginError::Offline);
    }
    let binary = entry
        .binary_for_current_platform()
        .ok_or_else(|| {
            PluginError::Manifest(format!(
                "{provider_id} {} has no binary for this platform",
                entry.version
            ))
        })?;

    let bytes = download_bytes(binary, &mut progress).await?;
    install_verified(provider_id, &entry.version, binary, bytes)
}

/// Gate the bytes of a plugin binary and install them into the version
/// cache. The two halves of an install that do not depend on where the
/// bytes came from, shared by the download above and the offline
/// bundle's seed import (`super::seed`), so an embedded copy is trusted
/// on exactly the terms a downloaded one is.
pub(crate) fn install_verified(
    provider_id: &str,
    version: &str,
    binary: &PlatformBinary,
    bytes: Vec<u8>,
) -> Result<PathBuf, PluginError> {
    // Gate 1: SHA-256. Cheap, catches a corrupted / truncated
    // transfer before the more expensive signature check.
    let digest = to_hex(&Sha256::digest(&bytes));
    if !digest.eq_ignore_ascii_case(&binary.sha256) {
        return Err(PluginError::Integrity(format!(
            "sha256 mismatch: manifest says {}, downloaded bytes hash to {digest}",
            binary.sha256
        )));
    }

    // Gate 2: Ed25519 signature against a baked-in trust anchor.
    verify::verify(&bytes, &binary.signature)?;

    write_verified(provider_id, version, binary, &bytes)
}

/// The write half of an install, after both gates passed: atomic write
/// into the version dir, the detached signature beside the binary, the
/// retention prune. Takes the anchors as already checked so the seed
/// import's test can drive it with a generated key.
pub(crate) fn write_verified(
    provider_id: &str,
    version: &str,
    binary: &PlatformBinary,
    bytes: &[u8],
) -> Result<PathBuf, PluginError> {
    // Both gates passed, write atomically into the version dir. The
    // sequence is `create_new` (refuses an existing partial), write,
    // `sync_all` (the file's data + metadata reach the disk), rename
    // (the dir entry flip is atomic on POSIX), and finally `fsync`
    // the parent dir on Unix so the rename itself survives a power
    // loss. Without `sync_all` the rename could land before the data
    // and we'd boot with a zero-length-but-verified-named binary.
    let dir = cache::version_dir(provider_id, version)?;
    std::fs::create_dir_all(&dir)?;
    let final_path = cache::binary_path(provider_id, version)?;
    let tmp_path = dir.join(format!("{}.tmp", cache::binary_name(provider_id)));
    // Clear any orphan from a previous crashed install before
    // `create_new` would otherwise reject the path.
    let _ = std::fs::remove_file(&tmp_path);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    set_executable(&tmp_path)?;
    std::fs::rename(&tmp_path, &final_path)?;
    // Persist the detached signature next to the binary so the host
    // can re-verify the cached file at spawn time (closes the gap
    // between install-time verification and a later tampered cache).
    std::fs::write(
        final_path.with_extension("sig"),
        binary.signature.as_bytes(),
    )?;
    #[cfg(unix)]
    {
        // fsync the parent directory so the rename itself is durable.
        // Best-effort: a failure here doesn't undo a successful
        // verify+rename, just leaves the install at the kernel-cache
        // level until the next sync.
        if let Ok(dir_file) = std::fs::File::open(&dir) {
            let _ = dir_file.sync_all();
        }
    }

    // Best-effort retention prune, a failure here doesn't invalidate
    // the install that just succeeded.
    if let Err(e) = cache::cleanup_keep_last_two(provider_id) {
        tracing::warn!(
            target = "oryxis::plugins",
            provider = %provider_id,
            error = %e,
            "plugin cache prune failed after install"
        );
    }

    Ok(final_path)
}

/// Stream the binary into memory, firing `progress` per chunk. Held
/// fully in memory (~25 MB for AWS) because the Ed25519 gate needs
/// every byte anyway, the same trade-off `update.rs` already makes
/// for the ~80 MB app installers.
async fn download_bytes(
    binary: &PlatformBinary,
    progress: &mut impl FnMut(u64, u64),
) -> Result<Vec<u8>, PluginError> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Oryxis/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .https_only(true)
        .build()
        .map_err(|e| PluginError::Download(e.to_string()))?;
    let resp = get_github(&client, &binary.url)
        .await
        .map_err(PluginError::Download)?;

    // Hard cap on the body size: a malicious or mistakenly-large
    // Content-Length (or the manifest's `size` field if Content-Length
    // is missing) would let us pre-allocate or stream gigabytes. AWS
    // plugin is ~25 MB today; 200 MB leaves headroom without giving
    // an attacker an OOM lever.
    const MAX_PLUGIN_BYTES: u64 = 200 * 1024 * 1024;
    let total = resp.content_length().unwrap_or(binary.size);
    if total > MAX_PLUGIN_BYTES {
        return Err(PluginError::Download(format!(
            "plugin binary advertises {total} bytes, exceeds {MAX_PLUGIN_BYTES} byte ceiling"
        )));
    }
    let mut buf: Vec<u8> = Vec::with_capacity(total as usize);
    let mut stream = resp.bytes_stream();
    progress(0, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| PluginError::Download(e.to_string()))?;
        if (buf.len() as u64).saturating_add(chunk.len() as u64) > MAX_PLUGIN_BYTES {
            return Err(PluginError::Download(format!(
                "plugin binary exceeded {MAX_PLUGIN_BYTES} byte ceiling mid-stream"
            )));
        }
        buf.extend_from_slice(&chunk);
        progress(buf.len() as u64, total.max(buf.len() as u64));
    }
    Ok(buf)
}

/// Lowercase hex encoding. Rolled by hand rather than pulling a
/// `hex` crate for one helper, consistent with the rest of the
/// codebase's "tiny helper over a dependency" preference.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Mark a freshly written plugin binary executable. No-op on
/// Windows, where executability is decided by file extension.
fn set_executable(path: &std::path::Path) -> Result<(), PluginError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_hex_matches_known_vectors() {
        assert_eq!(to_hex(&[0x00, 0xff, 0x10]), "00ff10");
        assert_eq!(to_hex(&[]), "");
        // SHA-256 of the empty input, the canonical sanity vector.
        assert_eq!(
            to_hex(&Sha256::digest(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// The tracked catalog is what every install now reads, and a
    /// release workflow writes it unattended. A file that stops
    /// parsing, or whose `provider_id` stops matching its name, would
    /// send every user of that plugin down the slow fallback with no
    /// louder symptom than a debug log line, so CI reads them here.
    #[test]
    fn tracked_catalog_files_parse_and_self_identify() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("plugins");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).expect("plugins/ directory") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            let body = std::fs::read_to_string(&path).expect("read catalog");
            let manifest = PluginManifest::parse(&body)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                manifest.provider_id, stem,
                "{} declares a different provider_id",
                path.display()
            );
            assert!(
                !manifest.versions.is_empty(),
                "{} carries no versions",
                path.display()
            );
            for v in &manifest.versions {
                assert!(
                    !v.binaries.is_empty(),
                    "{} {} carries no binaries",
                    path.display(),
                    v.version
                );
                assert!(
                    !v.protocol_versions.is_empty(),
                    "{} {} declares no protocol version, so the host filter \
                     can never intersect it",
                    path.display(),
                    v.version
                );
            }
            seen += 1;
        }
        // A directory that silently empties out would pass every
        // assertion above.
        assert!(seen >= 6, "expected one catalog per provider, found {seen}");
    }
}

//! Auto-update, queries GitHub releases on startup, prompts the user if a
//! newer version is available, downloads the platform artifact, and applies
//! it so the app can relaunch on the new version. Installed copies hand off
//! to the platform installer; copies nobody installed (the Windows portable
//! zip, a Linux AppImage, the nightly bare binary) swap themselves in place.
//!
//! Flow:
//!   1. `check_latest_release()`, async HTTP GET to GitHub releases/latest
//!   2. UI compares `tag_name` against `env!("CARGO_PKG_VERSION")`; if newer
//!      and not in `skipped_version`, shows a modal with 3 options:
//!        - **Skip this version** → persists tag into vault `settings` table
//!        - **Remind me later** → dismisses, asks next launch
//!        - **Update now** → triggers `download_installer` + `launch_installer_and_exit`
//!   3. During download, the UI shows a progress bar via streaming bytes.

use std::path::PathBuf;

/// Hard-coded release repo, kept in one place so publishing the app to a
/// fork or mirror requires a single edit.
pub const RELEASE_REPO: &str = "wilsonglasser/oryxis";

/// The release stream the auto-updater follows. Persisted as the
/// `update_channel` setting (`"stable"` / `"nightly"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateChannel {
    #[default]
    Stable,
    Nightly,
}

impl UpdateChannel {
    pub fn from_setting(s: &str) -> Self {
        match s {
            "nightly" => Self::Nightly,
            _ => Self::Stable,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }
}

/// Selectable channels for the settings picker, in display order.
pub const UPDATE_CHANNELS: [UpdateChannel; 2] = [UpdateChannel::Stable, UpdateChannel::Nightly];

// `pick_list` requires its option type to implement `Display` even when a
// mapper closure handles the visible label, so provide a plain fallback.
// The settings picker maps through i18n; this is only the default.
impl std::fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Stable => "Stable",
            Self::Nightly => "Nightly",
        })
    }
}

/// Channel this binary was built for, baked in by `build.rs`. Stable for
/// tagged releases and local builds; nightly only for the rolling CI
/// build. Used so a user who flips back to the stable channel from a
/// nightly binary is offered a clean stable build instead of being
/// stranded (the nightly's `CARGO_PKG_VERSION` would read as "not newer").
pub fn build_channel() -> UpdateChannel {
    UpdateChannel::from_setting(env!("ORYXIS_CHANNEL"))
}

/// How an update is applied once downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateArtifact {
    /// A platform installer (NSIS / AppImage / tarball) handed off to the
    /// OS. The stable channel's mechanism for installed copies.
    Installer,
    /// A bare executable that replaces the running binary in place. The
    /// nightly channel's mechanism, no installer is published for it.
    Binary,
    /// The Windows portable zip (`oryxis-windows-<arch>.zip`, a bare
    /// signed exe inside). Extracted, then the exe swaps the running
    /// binary through the same helper the nightly channel uses: handing
    /// a portable user the NSIS installer would lay down a SECOND,
    /// installed copy instead of updating the one they run (issue #180).
    PortableArchive,
    /// A stable AppImage replacing the running image file in place. The
    /// swap target is `$APPIMAGE` (the image on disk), never
    /// `current_exe` (the read-only mounted squashfs).
    AppImage,
}

/// Why an update check failed, kept separate from "no update available"
/// so the UI can report the truth instead of claiming up-to-date while
/// the network is down or firewalled (issue #38).
#[derive(Debug, Clone)]
pub enum UpdateError {
    /// DNS / connect / timeout / TLS failure, with a concise root cause.
    Network(String),
    /// Non-2xx HTTP status from the GitHub API.
    Http(u16),
    /// Payload didn't contain the expected fields.
    Parse,
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::Network(cause) => write!(f, "{cause}"),
            UpdateError::Http(status) => write!(f, "HTTP {status}"),
            UpdateError::Parse => write!(f, "unexpected API response"),
        }
    }
}

/// Settings > About status line for the manual update check. An enum
/// (not a pre-rendered string) so the view picks color + i18n at render
/// time and language switches don't strand a stale English string.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Failed(String),
}

/// Boil a reqwest error chain down to its root cause, the part the user
/// can act on ("failed to lookup address", "connection refused", ...).
fn concise_cause(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "timeout".to_string();
    }
    let mut src: &dyn std::error::Error = e;
    while let Some(inner) = src.source() {
        src = inner;
    }
    src.to_string()
}

/// Release metadata extracted from the GitHub API payload.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    /// Version without the leading `v` (e.g. `0.3.2`), or `nightly
    /// (<sha>)` for the nightly channel.
    pub version: String,
    /// HTML page for the release (for "What's new").
    pub html_url: String,
    /// Release notes body (markdown), preview shown in the modal.
    pub body: String,
    /// Download URL for the installer asset matching this platform.
    pub installer_url: Option<String>,
    /// Installer file name (used when saving to temp).
    pub installer_name: Option<String>,
    /// Whether to launch an installer or swap the binary in place.
    pub artifact: UpdateArtifact,
}

/// An update whose artifact is downloaded and waiting to be applied.
///
/// Kept apart from the offer (`Oryxis::pending_update`) so declining
/// the restart keeps the download: the next "Update now" for the same
/// version installs it instead of fetching it again, and Settings >
/// About offers the restart while it waits.
#[derive(Debug, Clone)]
pub struct ReadyUpdate {
    pub info: UpdateInfo,
    pub path: std::path::PathBuf,
}

/// Query the GitHub API for an available update on the given channel.
/// `Ok(None)` means genuinely up to date; failures (network, HTTP,
/// parse) come back as `Err` so callers can distinguish. The silent
/// boot check logs and ignores errors; the manual check surfaces them.
pub async fn check_latest_release(
    channel: UpdateChannel,
) -> Result<Option<UpdateInfo>, UpdateError> {
    match channel {
        UpdateChannel::Stable => check_stable().await,
        UpdateChannel::Nightly => check_nightly().await,
    }
}

/// Fetch a release JSON payload from a `releases/...` API path.
async fn fetch_release(path: &str) -> Result<serde_json::Value, UpdateError> {
    let url = format!("https://api.github.com/repos/{RELEASE_REPO}/{path}");
    let client = reqwest::Client::builder()
        .user_agent(concat!("Oryxis/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .https_only(true)
        .build()
        .map_err(|e| UpdateError::Network(concise_cause(&e)))?;
    // Mirror-aware: configured mirror first, direct as the fallback
    // (see `crate::net_mirror`); the Ed25519 gate in the download
    // path keeps any mirror untrusted.
    let mut last = UpdateError::Parse;
    for candidate in crate::net_mirror::candidates(&url) {
        match client.get(&candidate).send().await {
            Ok(resp) if resp.status().is_success() => {
                // A 200 with a bad or oversized body (a captive portal /
                // block page answering 200, or a hostile mirror serving a
                // giant payload to OOM us) must fall through to the next
                // candidate, not abort the whole check: that is exactly
                // the network the Auto fallback exists for.
                match read_capped_text(resp, MAX_RELEASE_JSON).await {
                    Ok(body) => match serde_json::from_str(&body) {
                        Ok(v) => return Ok(v),
                        Err(_) => last = UpdateError::Parse,
                    },
                    Err(e) => last = e,
                }
            }
            Ok(resp) => last = UpdateError::Http(resp.status().as_u16()),
            Err(e) => last = UpdateError::Network(concise_cause(&e)),
        }
    }
    Err(last)
}

/// Release JSON is a few KB; cap the read at 1 MiB so an untrusted
/// mirror can't stream an unbounded body as an OOM lever.
const MAX_RELEASE_JSON: usize = 1024 * 1024;

/// Read a response body to a `String` with a hard byte cap, streaming so
/// the cap bites before the whole payload is buffered. Over-cap or
/// non-UTF-8 is reported as `Parse` (the caller then tries the next
/// mirror candidate).
async fn read_capped_text(
    mut resp: reqwest::Response,
    cap: usize,
) -> Result<String, UpdateError> {
    let mut buf = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| UpdateError::Network(concise_cause(&e)))?
    {
        if buf.len() + chunk.len() > cap {
            return Err(UpdateError::Parse);
        }
        buf.extend_from_slice(&chunk);
    }
    String::from_utf8(buf).map_err(|_| UpdateError::Parse)
}

/// Stable channel: the newest tagged release. Normally only offered when
/// strictly newer than the running version, but a binary built on the
/// nightly channel always gets offered the latest stable so flipping the
/// channel toggle back actually lands the user on a stable build.
async fn check_stable() -> Result<Option<UpdateInfo>, UpdateError> {
    let json = fetch_release("releases/latest").await?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or(UpdateError::Parse)?
        .trim_start_matches('v')
        .to_string();
    let running_nightly = build_channel() == UpdateChannel::Nightly;
    if !running_nightly && !is_newer(&tag, env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .ok_or(UpdateError::Parse)?
        .to_string();
    let body = json.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let artifact = stable_artifact();
    let (installer_url, installer_name) = pick_asset(&json, artifact);
    Ok(Some(UpdateInfo {
        version: tag,
        html_url,
        body,
        installer_url,
        installer_name,
        artifact,
    }))
}

/// How a stable update applies on this machine. Installer everywhere,
/// with two in-place exceptions: a Windows portable copy (no NSIS
/// uninstaller beside the exe) swaps its binary from the portable zip,
/// and a Linux AppImage replaces the image file it was launched from.
fn stable_artifact() -> UpdateArtifact {
    if is_portable_install() {
        UpdateArtifact::PortableArchive
    } else if appimage_path().is_some() {
        UpdateArtifact::AppImage
    } else {
        UpdateArtifact::Installer
    }
}

/// Whether this Windows build runs as the portable zip: unpackaged (the
/// MSIX probe already refuses self-update elsewhere, but WindowsApps
/// also has no uninstaller, so it would read as portable here) and with
/// no NSIS uninstaller beside the exe. Both installers write
/// `uninstall.exe` into `$INSTDIR`, so its absence is a local,
/// path-independent signal that survives custom install directories.
#[cfg(target_os = "windows")]
pub(crate) fn is_portable_install() -> bool {
    if crate::packaged::is_packaged() {
        return false;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| !dir.join("uninstall.exe").exists()))
        .unwrap_or(false)
}

/// Portable is a Windows-zip concept; every other platform's stable
/// artifact decision goes through the AppImage probe or the installer.
#[cfg(not(target_os = "windows"))]
pub(crate) fn is_portable_install() -> bool {
    false
}

/// The AppImage file this process was launched from, if any. The
/// AppImage runtime exports `APPIMAGE` with the absolute path of the
/// image on disk; `current_exe` inside the mounted squashfs is
/// read-only and useless as a swap target.
#[cfg(target_os = "linux")]
pub(crate) fn appimage_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("APPIMAGE")?);
    (p.is_absolute() && p.is_file()).then_some(p)
}

/// AppImage is Linux-only; elsewhere the probe never matches.
#[cfg(not(target_os = "linux"))]
pub(crate) fn appimage_path() -> Option<PathBuf> {
    None
}

/// Nightly channel: the rolling `nightly-latest` prerelease. Version
/// numbers don't move between nightlies, so "newer" means a different
/// target commit than the one baked into this binary. `/releases/latest`
/// skips prereleases, hence the explicit tag lookup.
///
/// The tag was `nightly` until 2026-07-16, when a release published
/// under GitHub's "release immutability" burned that name permanently
/// (a tag ever used by an immutable release can never be recreated).
/// Binaries older than the rename get a 404 here and fall back to the
/// mirror snapshot, which kept the `releases/nightly.json` path.
async fn check_nightly() -> Result<Option<UpdateInfo>, UpdateError> {
    let json = fetch_release("releases/tags/nightly-latest").await?;
    let remote_sha = nightly_commit(&json).ok_or(UpdateError::Parse)?;
    let local_sha = env!("ORYXIS_GIT_SHA");
    // Dev build with no embedded SHA: can't compare, so never nag.
    if local_sha == "unknown" || commit_eq(&remote_sha, local_sha) {
        return Ok(None);
    }
    let html_url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .ok_or(UpdateError::Parse)?
        .to_string();
    let body = json.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let (installer_url, installer_name) = pick_asset(&json, UpdateArtifact::Binary);
    let short: String = remote_sha.chars().take(8).collect();
    Ok(Some(UpdateInfo {
        version: format!("nightly ({short})"),
        html_url,
        body,
        installer_url,
        installer_name,
        artifact: UpdateArtifact::Binary,
    }))
}

/// Extract the commit the nightly release points at. The publish job
/// creates the tag with `--target <full-sha>`, so `target_commitish`
/// usually carries it; fall back to the short SHA in the release title
/// (`Nightly (abcdef12)`).
fn nightly_commit(json: &serde_json::Value) -> Option<String> {
    if let Some(tc) = json.get("target_commitish").and_then(|v| v.as_str())
        && tc.len() >= 7
        && tc.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Some(tc.to_string());
    }
    let name = json.get("name").and_then(|v| v.as_str())?;
    let start = name.find('(')? + 1;
    let end = name[start..].find(')')? + start;
    let sha = &name[start..end];
    (sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit())).then(|| sha.to_string())
}

/// Compare two commit SHAs by their common-length prefix, so a short SHA
/// (8 hex from a title) matches the full 40-hex form.
fn commit_eq(a: &str, b: &str) -> bool {
    let n = a.len().min(b.len()).min(40);
    n >= 7 && a[..n].eq_ignore_ascii_case(&b[..n])
}

/// Strict "lhs > rhs" comparison over semantic-ish versions (major.minor.patch,
/// extra segments ignored). Returns false on parse failure so we never
/// prompt for a broken tag.
fn is_newer(lhs: &str, rhs: &str) -> bool {
    fn parse(s: &str) -> [u32; 3] {
        let mut out = [0u32; 3];
        for (i, seg) in s.split('.').take(3).enumerate() {
            let num: u32 = seg
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0);
            out[i] = num;
        }
        out
    }
    parse(lhs) > parse(rhs)
}

fn pick_asset(
    json: &serde_json::Value,
    artifact: UpdateArtifact,
) -> (Option<String>, Option<String>) {
    let assets = match json.get("assets").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return (None, None),
    };
    // Every asset ships with a detached signature and a checksum whose
    // names EXTEND the asset's own (`oryxis-setup-x86_64.exe.sig`), so
    // the substring match accepts them for both channels and only asset
    // ordering keeps the real installer winning. Exclude them
    // explicitly, everywhere: picking a sidecar downloads a base64
    // signature as the installer (verification then fails, so the cost
    // is a dead update, not code execution, but it is dead for every
    // user of that release).
    let mut exclude = vec![".sig", ".sha256"];
    let want = match artifact {
        // The AppImage IS this platform's stable asset, so both stable
        // shapes share the fragment table; what differs is only how the
        // download is applied.
        UpdateArtifact::Installer | UpdateArtifact::AppImage => {
            exclude.extend(platform_asset_exclude());
            platform_asset_fragment()
        }
        UpdateArtifact::PortableArchive => portable_zip_fragment(),
        UpdateArtifact::Binary => nightly_asset_fragment(),
    };
    for a in assets {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let lname = name.to_lowercase();
        if !want.iter().all(|w| lname.contains(w)) {
            continue;
        }
        if exclude.iter().any(|w| lname.contains(w)) {
            continue;
        }
        let url = a.get("browser_download_url").and_then(|v| v.as_str()).map(|s| s.to_string());
        return (url, Some(name.to_string()));
    }
    (None, None)
}

/// On Windows we ship two installers: `oryxis-setup-x86_64.exe` (system,
/// `Program Files`, requires UAC) and `oryxis-user-setup-x86_64.exe`
/// (per-user, `%LOCALAPPDATA%`, no UAC). Pick the one matching the
/// running install so the auto-update preserves scope. On other
/// platforms the function returns `false` (no per-user concept).
#[cfg(target_os = "windows")]
pub(crate) fn is_per_user_install() -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let local = match std::env::var_os("LOCALAPPDATA") {
        Some(v) => std::path::PathBuf::from(v),
        None => return false,
    };
    let exe_lc = exe.to_string_lossy().to_lowercase();
    let local_lc = local.to_string_lossy().to_lowercase();
    exe_lc.starts_with(&local_lc)
}

/// Substrings we expect inside the asset filename for the current
/// platform's INSTALLED stable artifact (portable Windows copies match
/// the zip via [`portable_zip_fragment`] instead). The release pipeline
/// emits, per architecture:
///   • Windows x64:    `oryxis-setup-x86_64.exe` (NSIS, system / UAC)
///                     `oryxis-user-setup-x86_64.exe` (NSIS, per-user)
///   • Windows arm64:  `oryxis-setup-aarch64.exe` (NSIS, system / UAC)
///                     `oryxis-user-setup-aarch64.exe` (NSIS, per-user)
///                     `oryxis-windows-aarch64.zip` (portable fallback)
///   • macOS arm64:    `oryxis-macos-aarch64.tar.gz`
///   • Linux x64:      `oryxis-linux-x86_64.AppImage`
///   • Linux arm64:    `oryxis-linux-aarch64.AppImage`
///
/// We match by the most discriminating combination per platform, so a
/// future asset rename in only one of those slots doesn't silently
/// break the rest. Returns the empty list for platforms we don't ship
/// a per-arch installer for, the caller surfaces "no installer
/// asset for this platform" so the user falls back to manual install.
fn platform_asset_fragment() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            #[cfg(target_os = "windows")]
            {
                if is_per_user_install() {
                    return vec!["user-setup", "x86_64", ".exe"];
                }
            }
            vec!["setup", "x86_64", ".exe"]
        } else if cfg!(target_arch = "aarch64") {
            #[cfg(target_os = "windows")]
            {
                if is_per_user_install() {
                    return vec!["user-setup", "aarch64", ".exe"];
                }
            }
            vec!["setup", "aarch64", ".exe"]
        } else {
            vec![]
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            vec!["macos", "aarch64", ".tar.gz"]
        } else {
            vec![]
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "x86_64") {
            vec!["linux", "x86_64", ".appimage"]
        } else if cfg!(target_arch = "aarch64") {
            vec!["linux", "aarch64", ".appimage"]
        } else {
            vec![]
        }
    } else {
        vec![]
    }
}

/// Substrings that disqualify an otherwise matching asset. Used to keep
/// the Windows system fragment (`["setup", "<arch>", ".exe"]`) from
/// accidentally picking up `oryxis-user-setup-<arch>.exe`, which
/// satisfies all three substrings. Only the system path needs an
/// exclude rule, `user-setup` is already specific enough on its own.
fn platform_asset_exclude() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        #[cfg(target_os = "windows")]
        {
            if is_per_user_install() {
                return vec![];
            }
        }
        return vec!["user-setup"];
    }
    vec![]
}

/// Substrings identifying the Windows portable zip for this arch
/// (`oryxis-windows-<arch>.zip`, the bare signed exe plus docs). Only
/// reachable on Windows, where the portable-install probe selects the
/// `PortableArchive` artifact.
fn portable_zip_fragment() -> Vec<&'static str> {
    if cfg!(target_arch = "x86_64") {
        vec!["windows", "x86_64", ".zip"]
    } else if cfg!(target_arch = "aarch64") {
        vec!["windows", "aarch64", ".zip"]
    } else {
        vec![]
    }
}

/// Substrings identifying this platform's bare-binary nightly asset. The
/// nightly workflow publishes, per platform:
///   • Linux:    `oryxis-nightly-linux-<arch>.bin`
///   • macOS:    `oryxis-nightly-macos-aarch64.bin`
///   • Windows:  `oryxis-nightly-windows-<arch>.exe`
/// The `.bin` / `.exe` suffix keeps the matcher from grabbing the
/// `.tar.gz` / `.zip` archives published under the same name stem.
fn nightly_asset_fragment() -> Vec<&'static str> {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "x86_64") {
            vec!["nightly", "windows", "x86_64", ".exe"]
        } else if cfg!(target_arch = "aarch64") {
            vec!["nightly", "windows", "aarch64", ".exe"]
        } else {
            vec![]
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            vec!["nightly", "macos", "aarch64", ".bin"]
        } else {
            vec![]
        }
    } else if cfg!(target_os = "linux") {
        if cfg!(target_arch = "x86_64") {
            vec!["nightly", "linux", "x86_64", ".bin"]
        } else if cfg!(target_arch = "aarch64") {
            vec!["nightly", "linux", "aarch64", ".bin"]
        } else {
            vec![]
        }
    } else {
        vec![]
    }
}

/// Download the installer to a temp file, streaming chunks straight to
/// disk (an ~80 MB body no longer sits in RAM during the transfer) and
/// reporting real progress through the closure.
///
/// The artifact's detached Ed25519 signature (the sibling `<asset>.sig`
/// release asset, published by the release/nightly workflows) is then
/// checked against the same trust anchors the plugin pipeline uses. A
/// missing or invalid signature deletes the download and aborts the
/// update: TLS alone is not the trust boundary for code we are about
/// to execute.
pub async fn download_installer(
    url: &str,
    file_name: &str,
    mut progress: impl FnMut(f32) + Send,
) -> Result<PathBuf, String> {
    use tokio::io::AsyncWriteExt as _;

    let client = reqwest::Client::builder()
        .user_agent(concat!("Oryxis/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(600))
        .https_only(true)
        .build()
        .map_err(|e| e.to_string())?;

    progress(0.0);
    // Mirror-aware: configured mirror first, direct fallback; the
    // Ed25519 check below keeps any mirror untrusted.
    let mut resp = None;
    let mut last = String::new();
    for candidate in crate::net_mirror::candidates(url) {
        match client.get(&candidate).send().await {
            Ok(r) if r.status().is_success() => {
                resp = Some(r);
                break;
            }
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
    }
    let mut resp = resp.ok_or(last)?;
    let total = resp.content_length().unwrap_or(0);
    // The asset name comes from unsigned release metadata (a hostile or
    // untrusted mirror can pick it), so it must never steer the write out
    // of the temp dir. Collapse to a bare file name and confirm the joined
    // path stays inside temp before creating/truncating anything.
    let temp = std::env::temp_dir();
    let base = std::path::Path::new(file_name)
        .file_name()
        .ok_or_else(|| format!("invalid update asset name: {file_name}"))?;
    let dest = temp.join(base);
    if dest.parent() != Some(temp.as_path()) {
        return Err(format!("invalid update asset name: {file_name}"));
    }
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            progress((downloaded as f32 / total as f32).min(0.99));
        }
    }
    file.flush().await.map_err(|e| e.to_string())?;
    drop(file);

    let sig_url = format!("{url}.sig");
    let mut sig_resp = None;
    let mut sig_last = String::new();
    for candidate in crate::net_mirror::candidates(&sig_url) {
        match client.get(&candidate).send().await {
            Ok(r) if r.status().is_success() => {
                sig_resp = Some(r);
                break;
            }
            Ok(r) => sig_last = format!("HTTP {}", r.status()),
            Err(e) => sig_last = e.to_string(),
        }
    }
    let Some(sig_resp) = sig_resp else {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(format!(
            "update signature missing ({sig_last} on {file_name}.sig)"
        ));
    };
    let sig_b64 = sig_resp.text().await.map_err(|e| e.to_string())?;
    let bytes = tokio::fs::read(&dest).await.map_err(|e| e.to_string())?;
    if let Err(e) = crate::plugins::verify::verify(&bytes, sig_b64.trim()) {
        let _ = tokio::fs::remove_file(&dest).await;
        return Err(format!("update signature verification failed: {e}"));
    }

    progress(1.0);
    Ok(dest)
}

/// Launch the platform installer and spawn-detach so it keeps running
/// after we exit. On Windows we go through `ShellExecuteW` so the
/// installer's manifest controls elevation: the system NSIS asks for
/// UAC, the per-user one runs as the current user without a prompt.
/// On macOS we open the mounted image; on Linux we open the file
/// manager so the user can run it.
pub fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let mut file: Vec<u16> = path.as_os_str().encode_wide().collect();
        file.push(0);

        let hinst = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW returns an HINSTANCE-shaped sentinel: values > 32
        // mean success, anything else is one of the documented error
        // codes (SE_ERR_ACCESSDENIED = 5 when the user declines UAC, etc).
        if (hinst as isize) <= 32 {
            return Err(format!("Failed to launch installer (ShellExecute={})", hinst as isize));
        }
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(path)
            .spawn()
            .map_err(|e| format!("Failed to open installer: {e}"))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Linux, best-effort: xdg-open file manager. install.sh expects the
        // user to run it manually.
        let _ = std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or_else(|| std::path::Path::new("/tmp")))
            .spawn();
    }
    Ok(())
}

/// Apply a downloaded bare binary (a nightly, or the exe extracted from
/// the stable portable zip): replace the running executable and
/// relaunch. Neither ships an installer, so there's nothing to hand
/// off, we swap in place. Returns once the new process is spawned; the
/// caller then closes the window so the old process exits and releases
/// the file.
pub fn apply_binary_update(downloaded: &std::path::Path) -> Result<(), String> {
    let current = std::env::current_exe().map_err(|e| format!("locate current exe: {e}"))?;

    #[cfg(unix)]
    swap_unix_binary(downloaded, &current)?;

    #[cfg(windows)]
    {
        // The helper waits for THIS pid and then replaces the exe, but a
        // second Oryxis window is its own process running the same file,
        // and Windows refuses to delete/replace a running image. The
        // move would burn its whole retry budget against the sibling's
        // lock and relaunch the old build, an admin token included (the
        // lock is not a permission problem). Refuse up front, while the
        // modal is still on screen to explain what to do.
        if !crate::tray_ipc::Primary::list_instances().is_empty() {
            return Err(crate::i18n::t("update_close_other_windows").to_string());
        }
        // A running .exe can't be overwritten in place, and on a
        // protected install dir (Program Files) it can't even be renamed
        // aside (the old code's `rename` failed with ERROR_ACCESS_DENIED
        // / os error 5). Hand the swap to a detached helper script that
        // waits for us to exit, then moves the new binary in and
        // relaunches. The helper only runs elevated when the install dir
        // isn't writable, so the common user-writable case never prompts.
        windows_self_replace(&current, downloaded)?;
    }

    Ok(())
}

/// Replace `target` with `downloaded` and relaunch it: stage beside the
/// target so the rename is same-filesystem and atomic, set the exec
/// bit, swap, spawn. Overwriting a running binary's path is fine on
/// Unix, the old inode stays alive for the still-running process.
#[cfg(unix)]
fn swap_unix_binary(
    downloaded: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    // Append rather than `with_extension`: an AppImage target would
    // otherwise stage as `Oryxis-x86_64.new`, shadowing nothing but
    // reading like a different artifact in a directory listing.
    let mut staged = target.as_os_str().to_os_string();
    staged.push(".new");
    let staged = PathBuf::from(staged);
    std::fs::copy(downloaded, &staged).map_err(|e| format!("stage binary: {e}"))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("set exec bit: {e}"))?;
    std::fs::rename(&staged, target).map_err(|e| format!("swap binary: {e}"))?;
    std::process::Command::new(target)
        .spawn()
        .map_err(|e| format!("relaunch: {e}"))?;
    Ok(())
}

/// Apply a downloaded stable AppImage: replace the image file this
/// process was launched from (`$APPIMAGE`) and relaunch it. Compiled
/// everywhere so the dispatch match stays cfg-free; only Linux ever
/// selects the artifact.
pub fn apply_appimage_update(downloaded: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let target = appimage_path()
            .ok_or_else(|| "not running from an AppImage".to_string())?;
        swap_unix_binary(downloaded, &target)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = downloaded;
        Err("AppImage update outside Linux".to_string())
    }
}

/// Pull the bare `oryxis.exe` out of a verified portable zip into a
/// fresh temp directory and hand back its path, ready for
/// [`apply_binary_update`]. The zip's other contents (README, logo)
/// are extracted alongside and swept with the directory. Compiled
/// everywhere for the same cfg-free-dispatch reason as
/// [`apply_appimage_update`]; only Windows ever selects the artifact.
pub fn extract_portable_exe(archive: &std::path::Path) -> Result<PathBuf, String> {
    let dest = std::env::temp_dir().join(format!(
        "oryxis-portable-update-{}",
        std::process::id()
    ));
    // A leftover from a previous attempt in this same process must not
    // shadow the fresh copy with a stale exe.
    let _ = std::fs::remove_dir_all(&dest);
    oryxis_archive::local::extract_archive(
        oryxis_archive::names::ArchiveKind::Zip,
        archive,
        &dest,
    )
    .map_err(|e| format!("unpack update: {e}"))?;
    let exe = dest.join("oryxis.exe");
    if !exe.is_file() {
        return Err("update archive does not contain oryxis.exe".to_string());
    }
    Ok(exe)
}

/// Windows in-place self-replace (nightly and portable-zip updates) via
/// a detached helper script. The
/// running process can't replace its own `.exe`, so we stage the new
/// binary, write a `.cmd` that waits for our PID to exit, moves the
/// staged file over the (now-unlocked) target with a short retry for
/// antivirus holds, relaunches, and deletes itself. The leftovers are
/// swept on the next boot by [`sweep_stale_binary`].
#[cfg(windows)]
fn windows_self_replace(
    current: &std::path::Path,
    downloaded: &std::path::Path,
) -> Result<(), String> {
    use std::io::Write;
    use std::os::windows::process::CommandExt;

    // CREATE_NO_WINDOW: run the helper console silently. The child
    // outlives us on its own; DETACHED_PROCESS is mutually exclusive
    // with CREATE_NO_WINDOW, so we don't combine them.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // A function item (not a closure) so it stays Copy and can be both
    // passed to `ok_or_else` and called directly afterwards.
    fn fail() -> String {
        crate::i18n::t("update_replace_failed").to_string()
    }
    let pid = std::process::id();
    let dir = current.parent().ok_or_else(fail)?;

    // Stage beside the target with a unique name so a leftover from a
    // previous attempt can't block us and a half-copy can't shadow the
    // live exe. If the install dir isn't writable, ERROR_ACCESS_DENIED
    // tells us elevation is needed: stage in TEMP and run the helper
    // elevated to move it into the protected dir.
    let staged_in_dir = dir.join(format!("oryxis-update-{pid}.tmp.exe"));
    let (staged, elevated) = match std::fs::copy(downloaded, &staged_in_dir) {
        Ok(_) => (staged_in_dir, false),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            tracing::info!(target = "oryxis::update", "install dir not writable, elevating self-replace");
            let temp = std::env::temp_dir().join(format!("oryxis-update-{pid}.tmp.exe"));
            std::fs::copy(downloaded, &temp).map_err(|e| {
                tracing::warn!(target = "oryxis::update", error = %e, "stage to temp failed");
                fail()
            })?;
            (temp, true)
        }
        Err(e) => {
            tracing::warn!(target = "oryxis::update", error = %e, "stage binary failed");
            return Err(fail());
        }
    };

    // `copy` (CopyFileEx) preserves the SOURCE mtime, so the staged file
    // inherits the download's timestamp, which can already be well past
    // the boot sweep's staleness threshold. Stamp it to now so its age
    // reflects staging time and a live helper's staged binary isn't swept
    // out from under it right after staging.
    if let Ok(f) = std::fs::File::options().write(true).open(&staged) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }

    // Failure contract: the helper runs after this process has exited,
    // so there is no UI left to report to. We drop a marker file before
    // handing off; every `move` attempt overwrites it with the real
    // error text, the success path deletes it, and the failure path
    // leaves the last error behind after relaunching the old binary.
    // If the marker still exists on the next boot the swap did not
    // land, whatever the failure shape (helper never ran, move kept
    // failing), and `take_update_failure` surfaces it (a QA'd loop:
    // the elevated swap once failed silently and the app just kept
    // re-offering the same nightly with no hint why).
    let marker = update_failure_marker();
    let _ = std::fs::write(&marker, "update helper did not run");

    // Volume-safe swap: never write over the LIVE exe directly. `move`
    // across volumes (staged in TEMP, install dir on another volume:
    // redirected TEMP, RAM disk) degrades to copy+delete and truncates
    // `{dst}` mid-copy, so a failed attempt would leave a corrupt binary
    // with no recovery. Instead copy into a same-volume sibling
    // `{dst}.new`, then atomically rename it over `{dst}`; the rename is a
    // same-volume MoveFileEx(REPLACE_EXISTING), so it either lands whole
    // or leaves the old `{dst}` intact. On any exhausted retry we clean up
    // `{dst}.new`, KEEP `{src}`, and relaunch the still-intact old `{dst}`
    // (never a half-written one); the marker keeps the last error for the
    // next-boot report.
    //
    // `enabledelayedexpansion` + `!tries!` so the retry counter updates
    // across loop iterations (plain `%tries%` is frozen at parse time).
    // The exit wait is bounded: if this process somehow never dies (a
    // hung teardown, a shell keeping the pid alive), an unbounded loop
    // would leave the helper spinning invisibly forever and the marker
    // reporting the misleading "helper did not run". Bail out after two
    // minutes with the real story in the marker and WITHOUT relaunching
    // (the process still being alive means its window is still there).
    let script = format!(
        "@echo off\r\n\
         setlocal enabledelayedexpansion\r\n\
         set waits=0\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq {pid}\" 2>nul | find \"{pid}\" >nul\r\n\
         if not errorlevel 1 (\r\n\
         set /a waits+=1\r\n\
         if !waits! geq 120 (\r\n\
         echo Oryxis process {pid} did not exit within 120s>\"{marker}\"\r\n\
         del \"{src}\" >nul 2>&1\r\n\
         del \"%~f0\" >nul 2>&1\r\n\
         exit /b 1\r\n\
         )\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         goto wait\r\n\
         )\r\n\
         set tries=0\r\n\
         :copy\r\n\
         copy /Y \"{src}\" \"{dst}.new\" >\"{marker}\" 2>&1\r\n\
         if not errorlevel 1 goto swap\r\n\
         set /a tries+=1\r\n\
         if !tries! lss 15 (\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         goto copy\r\n\
         )\r\n\
         del \"{dst}.new\" >nul 2>&1\r\n\
         goto launch\r\n\
         :swap\r\n\
         set tries=0\r\n\
         :swaploop\r\n\
         move /Y \"{dst}.new\" \"{dst}\" >\"{marker}\" 2>&1\r\n\
         if not errorlevel 1 goto ok\r\n\
         set /a tries+=1\r\n\
         if !tries! lss 15 (\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         goto swaploop\r\n\
         )\r\n\
         del \"{dst}.new\" >nul 2>&1\r\n\
         goto launch\r\n\
         :ok\r\n\
         del \"{src}\" >nul 2>&1\r\n\
         del \"{marker}\" >nul 2>&1\r\n\
         :launch\r\n\
         start \"\" \"{dst}\"\r\n\
         del \"%~f0\" >nul 2>&1\r\n",
        pid = pid,
        src = staged.display(),
        dst = current.display(),
        marker = marker.display(),
    );

    // Everything from here until the helper is running still executes
    // inside the live app, so failures are reported inline in the modal;
    // consume the marker on those paths or the next boot would re-report
    // an error the user already saw.
    let handoff = (|| -> Result<(), String> {
        let script_path = std::env::temp_dir().join(format!("oryxis-update-{pid}.cmd"));
        {
            let mut f = std::fs::File::create(&script_path).map_err(|e| {
                tracing::warn!(target = "oryxis::update", error = %e, "write helper script failed");
                fail()
            })?;
            f.write_all(script.as_bytes()).map_err(|e| {
                tracing::warn!(target = "oryxis::update", error = %e, "write helper script failed");
                fail()
            })?;
        }

        if elevated {
            // Protected dir: run the helper via ShellExecuteW "runas" so the
            // OS shows one UAC prompt (mirroring the system installer). The
            // relaunched app inherits the elevated token in this rare path;
            // the next manual launch returns to normal privileges.
            run_elevated_cmd(&script_path)?;
        } else {
            std::process::Command::new("cmd.exe")
                .arg("/c")
                .arg(&script_path)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| {
                    tracing::warn!(target = "oryxis::update", error = %e, "spawn helper failed");
                    fail()
                })?;
        }
        Ok(())
    })();
    if handoff.is_err() {
        let _ = std::fs::remove_file(&marker);
    }
    handoff
}

/// Path of the marker file the self-replace helper uses to report a
/// post-exit failure (see the contract note in [`windows_self_replace`]).
/// Lives in TEMP under a name the leftover sweep won't touch (it only
/// removes `.tmp.exe` / `.cmd`), so it survives until consumed.
#[cfg(windows)]
fn update_failure_marker() -> PathBuf {
    std::env::temp_dir().join("oryxis-update-failed.log")
}

/// Check-and-consume the failure marker a previous run's self-replace
/// left behind. Returns the helper's captured error text (e.g.
/// "Access is denied.") for logging; the caller shows the generic
/// localized message. Always `None` outside Windows, the Unix swap is
/// synchronous and reports its errors inline.
pub fn take_update_failure() -> Option<String> {
    #[cfg(windows)]
    {
        let marker = update_failure_marker();
        let detail = std::fs::read_to_string(&marker).ok()?;
        let _ = std::fs::remove_file(&marker);
        Some(detail.trim().to_string())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Run `cmd.exe /c <script>` elevated through `ShellExecuteW`'s "runas"
/// verb. Used only when the install dir needs admin rights to write.
#[cfg(windows)]
fn run_elevated_cmd(script: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = "cmd.exe\0".encode_utf16().collect();
    let mut params: Vec<u16> = "/c \"".encode_utf16().collect();
    params.extend(script.as_os_str().encode_wide());
    params.extend("\"".encode_utf16());
    params.push(0);

    let hinst = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };
    // > 32 means success; SE_ERR_ACCESSDENIED (5) lands here when the
    // user declines the UAC prompt.
    if (hinst as isize) <= 32 {
        tracing::warn!(target = "oryxis::update", code = hinst as isize, "elevated self-replace declined or failed");
        return Err(crate::i18n::t("update_replace_failed").to_string());
    }
    Ok(())
}

/// Clean up the leftovers a Windows in-place self-update (nightly or
/// portable zip) can leave behind: the legacy `.old.exe` (older
/// renaming scheme), the aborted `oryxis.exe.new` swap sibling, plus
/// the `oryxis-update-*` staged binary / helper script and the
/// `oryxis-portable-update-*` extraction dir the current scheme stages
/// beside the exe and in TEMP. All are consumed on a successful update; this sweeps the
/// remains of a failed or declined one. Best-effort and a no-op
/// everywhere else, called once on boot.
pub fn sweep_stale_binary() {
    #[cfg(windows)]
    {
        if let Ok(current) = std::env::current_exe() {
            let _ = std::fs::remove_file(current.with_extension("old.exe"));
            // The swap sibling from the volume-safe helper. Only touch it
            // when it's clearly stale, so a helper still mid-swap after a
            // manual relaunch isn't robbed of its new binary.
            let mut swap = current.clone().into_os_string();
            swap.push(".new");
            remove_if_stale(std::path::Path::new(&swap));
            if let Some(dir) = current.parent() {
                sweep_update_leftovers(dir);
            }
        }
        sweep_update_leftovers(&std::env::temp_dir());
    }
}

/// A helper that is still alive after this app relaunched can be in its
/// (max ~30s) wait/retry loop; a leftover from a genuinely dead run is
/// minutes old. Only sweep files older than this so a live self-replace
/// isn't robbed of its staged binary or script out from under it.
#[cfg(windows)]
const UPDATE_LEFTOVER_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(300);

#[cfg(windows)]
fn remove_if_stale(path: &std::path::Path) {
    let stale = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|age| age >= UPDATE_LEFTOVER_MIN_AGE)
        // No mtime / clock skew: leave it rather than risk killing a live one.
        .unwrap_or(false);
    if stale {
        let _ = std::fs::remove_file(path);
    }
}

/// Remove `oryxis-update-*.tmp.exe` / `oryxis-update-*.cmd` files and
/// `oryxis-portable-update-*` extraction dirs a stalled self-replace
/// left in `dir`, but only ones old enough to be from a dead run (see
/// [`remove_if_stale`]).
#[cfg(windows)]
fn sweep_update_leftovers(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("oryxis-update-")
            && (name.ends_with(".tmp.exe") || name.ends_with(".cmd"))
        {
            remove_if_stale(&entry.path());
        } else if name.starts_with("oryxis-portable-update-") {
            // The portable zip's extraction dir. Same staleness rule so a
            // live helper still copying out of it isn't swept mid-swap.
            let path = entry.path();
            let stale = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|age| age >= UPDATE_LEFTOVER_MIN_AGE)
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_eq_matches_full_and_short_prefixes() {
        let full = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678";
        // Identical full SHAs.
        assert!(commit_eq(full, full));
        // Short (8-hex title form) vs full: compare on the common prefix.
        assert!(commit_eq("a1b2c3d4", full));
        assert!(commit_eq(full, "A1B2C3D4")); // case-insensitive
        // Different commits.
        assert!(!commit_eq("a1b2c3d4", "ffffffff0000"));
        // Too short to trust (< 7 hex) never matches, guards against
        // accidental "everything is up to date" on a garbage value.
        assert!(!commit_eq("a1b", "a1b2c3d4"));
        assert!(!commit_eq("", full));
    }

    #[test]
    fn nightly_commit_prefers_hex_target_commitish() {
        let json = serde_json::json!({
            "target_commitish": "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
            "name": "Nightly (deadbeef)",
        });
        // A real hex commitish wins over the title.
        assert_eq!(
            nightly_commit(&json).as_deref(),
            Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"),
        );
    }

    #[test]
    fn nightly_commit_falls_back_to_title_when_commitish_is_a_branch() {
        // GitHub often returns the branch name, not a SHA, in
        // target_commitish; parse the short SHA out of the title instead.
        let json = serde_json::json!({
            "target_commitish": "main",
            "name": "Nightly (deadbeef)",
        });
        assert_eq!(nightly_commit(&json).as_deref(), Some("deadbeef"));
    }

    /// A release asset list shaped like a real stable release: both
    /// portable zips, their sidecars first (so ordering alone can't
    /// save the matcher), and the installers.
    fn stable_assets() -> serde_json::Value {
        serde_json::json!({
            "assets": [
                {"name": "oryxis-windows-x86_64.zip.sig",
                 "browser_download_url": "https://example.com/oryxis-windows-x86_64.zip.sig"},
                {"name": "oryxis-windows-x86_64.zip",
                 "browser_download_url": "https://example.com/oryxis-windows-x86_64.zip"},
                {"name": "oryxis-windows-aarch64.zip",
                 "browser_download_url": "https://example.com/oryxis-windows-aarch64.zip"},
                {"name": "oryxis-setup-x86_64.exe",
                 "browser_download_url": "https://example.com/oryxis-setup-x86_64.exe"},
                {"name": "oryxis-user-setup-x86_64.exe",
                 "browser_download_url": "https://example.com/oryxis-user-setup-x86_64.exe"},
                {"name": "oryxis-linux-x86_64.AppImage",
                 "browser_download_url": "https://example.com/oryxis-linux-x86_64.AppImage"},
            ]
        })
    }

    #[test]
    fn pick_asset_portable_archive_selects_arch_zip_never_a_sidecar() {
        let (url, name) = pick_asset(&stable_assets(), UpdateArtifact::PortableArchive);
        let expect = if cfg!(target_arch = "aarch64") {
            "oryxis-windows-aarch64.zip"
        } else {
            "oryxis-windows-x86_64.zip"
        };
        assert_eq!(name.as_deref(), Some(expect));
        assert_eq!(url.as_deref(), Some(format!("https://example.com/{expect}").as_str()));
    }

    #[test]
    fn pick_asset_appimage_shares_the_stable_fragment_table() {
        // Only meaningful where the stable asset IS the AppImage.
        if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            return;
        }
        let (_, name) = pick_asset(&stable_assets(), UpdateArtifact::AppImage);
        assert_eq!(name.as_deref(), Some("oryxis-linux-x86_64.AppImage"));
    }

    #[test]
    fn nightly_commit_none_when_unparseable() {
        let json = serde_json::json!({
            "target_commitish": "main",
            "name": "Nightly build",
        });
        assert!(nightly_commit(&json).is_none());
    }
}

//! The offline bundle: what sits NEXT TO THE EXECUTABLE.
//!
//! `oryxis-offline-<platform>-<arch>` is an additional release asset
//! carrying the app plus every plugin binary plus the pinned font
//! files, for a machine that never had a network. Its layout, relative
//! to the executable:
//!
//! ```text
//! oryxis[.exe]
//! offline-bundle              <- marker: offline mode on at first run
//! plugins/<provider>.json     <- the catalog entry the binary was pulled by
//! plugins/<binary>            <- oryxis-cloud-aws-plugin, oryxis-mcp, ...
//! fonts/<file>                <- the CJK faces and the terminal pack
//! ```
//!
//! Every consumer resolves the directory through [`dir`], which is the
//! ONE place that knows the answer is `None` under the harness sandbox:
//! a run there starts from first-run state whatever the machine looks
//! like, and `target/debug` is machine state the `$HOME` redirect
//! cannot reach (the rule `plugins::dev_binary_present` already
//! follows). Three separate `current_exe().parent()` probes would drift
//! on that rule.
//!
//! The embedded copies are a SEED, never a replacement for the download
//! path: plugins are IMPORTED into the ordinary cache
//! (`plugins::seed`), verified against the same Ed25519 anchors a
//! download is, and age out through the same update path once the
//! switch is off; fonts are read through from `fonts/` with the same
//! byte-length test the cache applies, because a pinned font is
//! content-addressed and never ages. The marker alone is also a valid
//! deployment: dropped next to an ordinary install, it turns offline
//! mode on for a fleet whose owner never wants the app on the network,
//! and it decides only the FIRST run (the settings row wins after
//! that).

use std::path::PathBuf;

/// The marker whose presence beside the executable turns offline mode
/// on at first run.
pub(crate) const MARKER: &str = "offline-bundle";

/// The directory the executable runs from, or `None` under the harness
/// sandbox (see the module doc) or when the executable path cannot be
/// resolved.
pub(crate) fn dir() -> Option<PathBuf> {
    #[cfg(feature = "harness")]
    if crate::harness::is_sandboxed() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    exe.parent().map(|p| p.to_path_buf())
}

/// Whether the offline marker sits next to the executable.
pub(crate) fn marker_present() -> bool {
    dir().is_some_and(|d| d.join(MARKER).is_file())
}

/// `plugins/` next to the executable, whether or not it exists.
pub(crate) fn plugins_dir() -> Option<PathBuf> {
    dir().map(|d| d.join("plugins"))
}

/// `fonts/` next to the executable, whether or not it exists.
pub(crate) fn fonts_dir() -> Option<PathBuf> {
    dir().map(|d| d.join("fonts"))
}

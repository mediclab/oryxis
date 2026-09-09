//! Offline mode: the app makes no request of its own.
//!
//! The line this switch draws is not "network" but WHOSE REQUEST IT IS.
//! What the app initiates on its own behalf goes quiet: the release
//! lookup and the installer download (`update`), the on-demand CJK
//! faces and the terminal font pack including the boot heal of the
//! configured font (`fonts`), the plugin catalog and binaries
//! (`plugins::download`, so the MCP install too), and the mirror probe
//! (`net_mirror`), since a mirror only ever routes those. What the user
//! asked for still travels: the hosts they dial, the AI endpoint and
//! key they typed, the sync transport and relay they configured, the
//! cloud accounts they signed into, the network tools target they
//! entered.
//!
//! The gate is in the ENGINE, not the callers: every one of those fetch
//! functions asks [`is_on`] before it dials and answers a TYPED refusal
//! (`UpdateError::Offline`, `PluginError::Offline`,
//! `fonts::FetchError::Offline`), so a surface that would have acted on
//! the result can say why it did not, in the active language, and a
//! new call site added later inherits the refusal instead of having to
//! remember it. A silenced fetch reports itself where it would have
//! acted (the font that cannot heal, the update that is not checked)
//! rather than failing mute, and nothing is deleted, so turning the
//! switch back off restores every feature without a reinstall.
//!
//! One build, one setting (`offline_mode`, default off), never a second
//! edition. Process-wide like `net_mirror::CHOICE`: seeded from the
//! settings table before the vault is unlocked (a locked vault's boot
//! fetches must already honor it), flipped by the Settings toggle, and
//! read by download tasks that touch it once per request. The offline
//! bundle turns it on at first run through `bundle::marker_present`.

use std::sync::atomic::{AtomicBool, Ordering};

/// The settings row. Read pre-unlock in `boot`, written by the Advanced
/// toggle and the onboarding features slide.
pub(crate) const SETTING_KEY: &str = "offline_mode";

static OFFLINE: AtomicBool = AtomicBool::new(false);

/// Publish the current answer for the whole process.
pub(crate) fn set(on: bool) {
    OFFLINE.store(on, Ordering::Relaxed);
}

/// Whether the app may make a request of its own right now.
pub(crate) fn is_on() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

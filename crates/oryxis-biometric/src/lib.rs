//! Local app-unlock backed by the operating system's biometric / keystore.
//!
//! # What this is (and is not)
//!
//! This is **local app unlock**, not SSH authentication and not a second
//! encryption factor. The vault is, and stays, encrypted with the
//! Argon2id key derived from the master password. Biometric unlock does
//! one thing: it stores the master password in an OS-protected secret
//! store and releases it only after a platform user-presence check, then
//! feeds it into the ordinary [`VaultStore::unlock`](../oryxis_vault)
//! path. A biometric-unlocked session is byte-for-byte equivalent to one
//! the user typed the password into, which is deliberate: it keeps
//! password-bearing sync working (the sync task re-opens the vault and
//! needs the plaintext password), where releasing only the derived key
//! would silently break it.
//!
//! # Security model
//!
//! The secret at rest is guarded by the platform, per user account:
//!
//! - **Windows** ([`WindowsHello`]): the master password lives in
//!   Credential Manager (per-user, DPAPI-backed) and every read is gated
//!   behind `UserConsentVerifier` (Windows Hello: face / fingerprint /
//!   PIN).
//! - **macOS** ([`TouchId`]): the master password lives in the login
//!   Keychain (per user, guarded by the Keychain's own code-identity
//!   ACL) and every read is gated behind a LocalAuthentication presence
//!   check: Touch ID, a paired Apple Watch, or the login password.
//! - **Linux** ([`SecretServiceStore`]): the freedesktop Secret Service
//!   (login keyring). Linux has no standard fingerprint API, so this is
//!   "OS keystore gated by the unlocked login session", not real
//!   biometry. The UI says so rather than implying a fingerprint.
//!
//! On Windows and macOS the prompt is raised by the app around the read;
//! the stored item is not itself bound to it. Binding the two is a
//! platform facility a freely distributable build cannot claim (on macOS
//! it means a data-protection Keychain item, whose entitlement is
//! restricted and therefore wants a paid developer identity), and what
//! it would add cover against is code already running as this user, on a
//! Mac already unlocked. Which is the same code that can read the master
//! password out of the running app's memory.
//!
//! Because the running app already holds the master password in memory
//! while unlocked (the sync path reads it), enrolling does not widen the
//! in-memory exposure; it only adds an at-rest copy under OS protection.
//! Trust therefore reduces to trusting the OS keystore for the logged-in
//! user, the same assumption every OS password manager makes.
//!
//! # Orchestration
//!
//! [`BiometricVault`] is the platform-independent orchestrator: it owns a
//! [`BiometricProvider`] and a per-vault `account` string (so two vaults
//! never collide in the shared store) and encodes the lifecycle the app
//! drives: enroll after a password unlock, retrieve to unlock, refresh on
//! a master-password rotation, disable on opt-out. The lifecycle is unit
//! tested against an in-memory mock, so a regression (e.g. a rotation
//! that forgets to update the stored secret) is caught without any OS
//! keystore present.

mod provider;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

pub use provider::{BioError, BiometricProvider, UnavailableProvider};

/// Return the platform's default biometric provider. On an unsupported
/// target this is [`UnavailableProvider`], whose `is_available()` is
/// always `false`, so the caller simply never offers the feature.
pub fn default_provider() -> Box<dyn BiometricProvider> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsHello::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::TouchId::new())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Box::new(linux::SecretServiceStore::new())
    }
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
    {
        Box::new(UnavailableProvider)
    }
}

/// Platform-independent orchestrator over a [`BiometricProvider`].
///
/// Holds the provider plus the per-vault `account` key. All the lifecycle
/// decisions (availability gating, enroll / retrieve / refresh / disable)
/// live here so they are testable against a mock, independent of any real
/// OS keystore.
pub struct BiometricVault {
    provider: Box<dyn BiometricProvider>,
    account: String,
}

impl BiometricVault {
    /// Build over the platform default provider for a given vault account.
    ///
    /// `account` must be stable per vault and distinct across vaults (the
    /// app derives it from the vault path) so entries in the shared OS
    /// store never collide.
    pub fn new(account: impl Into<String>) -> Self {
        Self {
            provider: default_provider(),
            account: account.into(),
        }
    }

    /// Build over an explicit provider. Used by tests to inject the mock.
    pub fn with_provider(provider: Box<dyn BiometricProvider>, account: impl Into<String>) -> Self {
        Self {
            provider,
            account: account.into(),
        }
    }

    /// Whether this platform/session can biometrically unlock at all. The
    /// UI must gate the whole feature (setting row + lock-screen button)
    /// on this so an unsupported target never advertises it.
    pub fn is_available(&self) -> bool {
        self.provider.is_available()
    }

    /// Store `master_password` under OS protection so future launches can
    /// release it after a presence check. Called right after a successful
    /// password unlock, when the plaintext is already in hand. Overwrites
    /// any previous enrollment for this account.
    pub fn enroll(&self, master_password: &str) -> Result<(), BioError> {
        if !self.is_available() {
            return Err(BioError::Unavailable);
        }
        self.provider.enroll(&self.account, master_password)
    }

    /// Raise the platform presence prompt and, on success, return the
    /// enrolled master password for feeding into `VaultStore::unlock`.
    /// `prompt` is the localized reason line shown in the OS dialog.
    /// Returns [`BioError::NotEnrolled`] when nothing is stored and
    /// [`BioError::Denied`] when the user cancels or fails verification.
    pub fn unlock_secret(&self, prompt: &str) -> Result<String, BioError> {
        if !self.is_available() {
            return Err(BioError::Unavailable);
        }
        self.provider.retrieve(&self.account, prompt)
    }

    /// Re-enroll after the master password changed (rotation / change
    /// password). Clearing first keeps stores that don't overwrite in
    /// place from stranding the old secret. Must be called in the same
    /// flow that rotates the password, or biometric unlock silently
    /// breaks with a stale secret.
    pub fn refresh(&self, new_master_password: &str) -> Result<(), BioError> {
        if !self.is_available() {
            return Err(BioError::Unavailable);
        }
        // Best-effort clear: a missing prior entry is not an error here.
        let _ = self.provider.clear(&self.account);
        self.provider.enroll(&self.account, new_master_password)
    }

    /// Forget the stored secret (opt-out, vault reset). Idempotent: a
    /// no-entry clear succeeds so the caller can call it unconditionally.
    pub fn disable(&self) -> Result<(), BioError> {
        self.provider.clear(&self.account)
    }
}

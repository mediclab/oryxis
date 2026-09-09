//! macOS provider: the master password lives in the login Keychain and
//! every read is gated by a LocalAuthentication presence check (Touch ID,
//! a paired Apple Watch, or the login password).
//!
//! # Why the presence check is the app's, not the item's
//!
//! The obvious shape is a Keychain item carrying a `SecAccessControl`
//! user-presence policy, so the release of the secret IS the Touch ID
//! prompt. That shape cannot ship here, and the reason is distribution,
//! not taste: an item with an access-control policy only lives in the
//! data-protection keychain, reaching that keychain requires a
//! `keychain-access-groups` entitlement, and that entitlement is
//! restricted, so it has to be allowlisted by a provisioning profile
//! embedded in the bundle, which needs a paid Apple Developer Program
//! membership. Without it every call answers `errSecMissingEntitlement`
//! (-34018) and the toggle can never turn on (PR #222, 2026-09-09). The
//! owner's decision is that Touch ID unlock must work on the ad-hoc
//! signed build we ship today, so the gate moved to the app.
//!
//! That leaves the exact shape Windows already has and that shipped with
//! this feature: raise the platform presence prompt first
//! (`UserConsentVerifier` there, `LAContext` here), then read the secret
//! out of the per-user OS store (Credential Manager there, the login
//! Keychain here). LocalAuthentication needs no entitlement of any kind.
//!
//! Two things follow, both deliberate. The gate is app logic, so an
//! attacker already running code AS this app on an unlocked Mac is not
//! stopped by it (they are equally unstopped on Windows, and on Linux
//! where the login keyring has no prompt at all). And the Keychain's own
//! ACL is keyed on the app's code identity, so an ad-hoc signed build
//! that changes identity on every update makes macOS ask for the login
//! password once after an update ("Always Allow" settles it).
//!
//! # Attribute hygiene
//!
//! Nothing here may carry a data-protection attribute:
//! `kSecAttrAccessControl`, `kSecAttrAccessible*`,
//! `kSecUseOperationPrompt` or `kSecUseDataProtectionKeychain`. Any of
//! them routes the item to the keychain we cannot reach and brings back
//! -34018, which is why that status is mapped to a message naming this
//! rule rather than printed as a bare number.
//!
//! The Security-framework entry points and the `kSec*` attribute keys are
//! declared here as externs against the framework's stable C ABI, rather
//! than pulled from a `-sys` crate whose exact Rust surface we cannot
//! compile-check from the Linux dev host. Only Core Foundation is used for
//! the CFString / CFData / CFDictionary plumbing. LocalAuthentication is
//! Objective-C and has no C ABI, so it goes through `objc2`, whose
//! generated bindings are already in the build (winit uses the family).

use block2::RcBlock;
use core_foundation::base::{CFType, TCFType};
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::string::CFStringRef;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAError, LAPolicy};

use crate::provider::{BioError, BiometricProvider};

/// Keychain service attribute; the per-vault account is the account
/// attribute, so two vaults are two distinct items.
const SERVICE: &str = "oryxis-vault-unlock";

// OSStatus codes we branch on (stable public values).
type OSStatus = i32;
const ERR_SEC_SUCCESS: OSStatus = 0;
const ERR_SEC_ITEM_NOT_FOUND: OSStatus = -25300;
const ERR_SEC_DUPLICATE_ITEM: OSStatus = -25299;
const ERR_SEC_USER_CANCELED: OSStatus = -128;
const ERR_SEC_AUTH_FAILED: OSStatus = -25293;
/// `errSecMissingEntitlement`: the call landed on the data-protection
/// keychain, which this build cannot reach. Nothing here asks for it, so
/// seeing it means a data-protection attribute crept into a query; the
/// message says so rather than leaving a bare number for the log.
const ERR_SEC_MISSING_ENTITLEMENT: OSStatus = -34018;

/// Presence policy: biometry, a paired Apple Watch, or the login
/// password. The biometry-only policy (`DeviceOwnerAuthenticationWith
/// Biometrics`) would refuse outright on a Mac with no Touch ID, where
/// the login password is a real presence check and the user asked for
/// one; the same reasoning that picked user-presence over
/// biometry-current-set when this feature shipped.
const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthentication;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: CFStringRef;
    static kSecClassGenericPassword: CFStringRef;
    static kSecAttrService: CFStringRef;
    static kSecAttrAccount: CFStringRef;
    static kSecValueData: CFStringRef;
    static kSecReturnData: CFStringRef;
    static kSecMatchLimit: CFStringRef;
    static kSecMatchLimitOne: CFStringRef;

    fn SecItemAdd(attributes: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemCopyMatching(query: CFTypeRef, result: *mut CFTypeRef) -> OSStatus;
    fn SecItemUpdate(query: CFTypeRef, attributes_to_update: CFTypeRef) -> OSStatus;
    fn SecItemDelete(query: CFTypeRef) -> OSStatus;
}

pub struct TouchId;

impl TouchId {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TouchId {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrap a static `kSec*` CFStringRef as an owned-by-get-rule `CFType` so it
/// can go into a `CFDictionary` of `CFType` pairs.
fn key(k: CFStringRef) -> CFType {
    unsafe { CFString::wrap_under_get_rule(k).as_CFType() }
}

/// The (service, account) pair every operation keys on. Deliberately
/// nothing else: see the attribute-hygiene note at the top.
fn base_pairs(account: &str) -> Vec<(CFType, CFType)> {
    vec![
        (key(unsafe { kSecClass }), key(unsafe { kSecClassGenericPassword })),
        (
            key(unsafe { kSecAttrService }),
            CFString::new(SERVICE).as_CFType(),
        ),
        (
            key(unsafe { kSecAttrAccount }),
            CFString::new(account).as_CFType(),
        ),
    ]
}

/// Map a Keychain `OSStatus` to the shared error type, naming the two
/// statuses whose bare number tells the reader nothing.
fn keychain_error(op: &str, status: OSStatus) -> BioError {
    match status {
        ERR_SEC_USER_CANCELED | ERR_SEC_AUTH_FAILED => BioError::Denied,
        ERR_SEC_MISSING_ENTITLEMENT => BioError::Backend(format!(
            "{op} failed: -34018 (errSecMissingEntitlement); the query reached \
             the data-protection keychain, which this build cannot use"
        )),
        other => BioError::Backend(format!("{op} failed: {other}")),
    }
}

/// Map an `LAError` code to the shared error type. A cancel (by the user,
/// the system or us) and a failed check are the same "no" to the caller,
/// which falls back to the typed password; anything else is a backend
/// fault worth logging verbatim.
fn presence_error(code: isize) -> BioError {
    match LAError(code) {
        LAError::UserCancel
        | LAError::UserFallback
        | LAError::SystemCancel
        | LAError::AppCancel
        | LAError::AuthenticationFailed => BioError::Denied,
        LAError::PasscodeNotSet | LAError::BiometryNotAvailable | LAError::BiometryNotEnrolled => {
            BioError::Unavailable
        }
        _ => BioError::Backend(format!("LAContext evaluatePolicy failed: {code}")),
    }
}

/// Raise the LocalAuthentication sheet and block until the user answers.
///
/// `evaluatePolicy` is asynchronous and calls back on a queue of its own,
/// so the reply is funnelled through a channel and waited on here. The
/// caller is already off the UI thread (the app drives `retrieve` in
/// `spawn_blocking`), which is what makes blocking legitimate. The
/// context is held for the whole wait on purpose: dropping it invalidates
/// the evaluation in flight and the reply comes back as `AppCancel`.
fn presence_check(reason: &str) -> Result<(), BioError> {
    let ctx = unsafe { LAContext::new() };

    // Preflight, so a Mac that cannot answer the policy at all reports
    // Unavailable instead of showing a sheet that is going to fail.
    if let Err(e) = unsafe { ctx.canEvaluatePolicy_error(POLICY) } {
        return Err(presence_error(e.code()));
    }

    let reason = NSString::from_str(reason);
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), BioError>>();
    // The reply block runs once, on LocalAuthentication's own thread. It
    // reads the error CODE and drops the pointer there; nothing
    // Objective-C escapes into the waiting thread.
    let block = RcBlock::new(move |ok: Bool, err: *mut NSError| {
        let outcome = if ok.as_bool() {
            Ok(())
        } else {
            let code = unsafe { err.as_ref() }.map(|e| e.code()).unwrap_or(0);
            Err(presence_error(code))
        };
        let _ = tx.send(outcome);
    });
    unsafe { ctx.evaluatePolicy_localizedReason_reply(POLICY, &reason, &block) };

    rx.recv().unwrap_or_else(|_| {
        Err(BioError::Backend(
            "LAContext evaluatePolicy answered nothing".into(),
        ))
    })
}

impl BiometricProvider for TouchId {
    fn is_available(&self) -> bool {
        // A real answer, not an assumption: `canEvaluatePolicy` is the
        // preflight LocalAuthentication exposes for exactly this, shows
        // no UI, and says no on a Mac with neither biometry nor a login
        // password. The app probes it once at boot and gates the whole
        // feature (setting row and lock-screen button) on the result.
        let ctx = unsafe { LAContext::new() };
        unsafe { ctx.canEvaluatePolicy_error(POLICY) }.is_ok()
    }

    fn enroll(&self, account: &str, secret: &str) -> Result<(), BioError> {
        let value = CFData::from_buffer(secret.as_bytes()).as_CFType();

        let mut pairs = base_pairs(account);
        pairs.push((key(unsafe { kSecValueData }), value.clone()));
        let dict = CFDictionary::from_CFType_pairs(&pairs);

        match unsafe { SecItemAdd(dict.as_CFTypeRef(), std::ptr::null_mut()) } {
            ERR_SEC_SUCCESS => Ok(()),
            // Replace in place rather than delete-then-add. The delete
            // needs the same Keychain authorization the read does, so on
            // a build whose code identity changed since the item was
            // written (every ad-hoc update) a refused delete used to
            // leave the add facing its own leftover item.
            ERR_SEC_DUPLICATE_ITEM => {
                let query = CFDictionary::from_CFType_pairs(&base_pairs(account));
                let update =
                    CFDictionary::from_CFType_pairs(&[(key(unsafe { kSecValueData }), value)]);
                match unsafe { SecItemUpdate(query.as_CFTypeRef(), update.as_CFTypeRef()) } {
                    ERR_SEC_SUCCESS => Ok(()),
                    other => Err(keychain_error("SecItemUpdate", other)),
                }
            }
            other => Err(keychain_error("SecItemAdd", other)),
        }
    }

    fn retrieve(&self, account: &str, prompt: &str) -> Result<String, BioError> {
        // Presence first, secret second: the same order Windows uses, and
        // the order that makes the prompt mean something. A refusal never
        // reaches the Keychain at all.
        presence_check(prompt)?;

        let mut pairs = base_pairs(account);
        pairs.push((key(unsafe { kSecReturnData }), cf_true()));
        pairs.push((
            key(unsafe { kSecMatchLimit }),
            key(unsafe { kSecMatchLimitOne }),
        ));

        let dict = CFDictionary::from_CFType_pairs(&pairs);
        let mut result: CFTypeRef = std::ptr::null_mut();
        let status = unsafe { SecItemCopyMatching(dict.as_CFTypeRef(), &mut result) };
        match status {
            ERR_SEC_SUCCESS if !result.is_null() => {
                let data = unsafe { CFData::wrap_under_create_rule(result as _) };
                String::from_utf8(data.to_vec())
                    .map_err(|e| BioError::Backend(format!("stored secret not UTF-8: {e}")))
            }
            ERR_SEC_ITEM_NOT_FOUND => Err(BioError::NotEnrolled),
            other => Err(keychain_error("SecItemCopyMatching", other)),
        }
    }

    fn clear(&self, account: &str) -> Result<(), BioError> {
        let dict = CFDictionary::from_CFType_pairs(&base_pairs(account));
        match unsafe { SecItemDelete(dict.as_CFTypeRef()) } {
            ERR_SEC_SUCCESS | ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            other => Err(keychain_error("SecItemDelete", other)),
        }
    }
}

/// `kCFBooleanTrue` as a `CFType` for the `kSecReturnData` flag.
fn cf_true() -> CFType {
    use core_foundation::boolean::CFBoolean;
    CFBoolean::true_value().as_CFType()
}

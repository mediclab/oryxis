//! Certificate attach / validate / viewer, split out of
//! `dispatch_keys`: the import form's certificate field, the cert
//! file dialog, the cert viewer overlay, cert removal, plus the
//! validation helpers shared with the import path. Called from
//! `handle_keys`.

#![allow(clippy::result_large_err)]

use iced::widget::text_editor;
use iced::Task;

use crate::app::{KeysMessage, Message, Oryxis};

impl Oryxis {
    pub(super) fn handle_keys_certs(
        &mut self,
        message: KeysMessage,
    ) -> Result<Task<Message>, KeysMessage> {
        match message {
            KeysMessage::KeyImportCertAction(action) => {
                let edited = action.is_edit();
                self.keys_ui.import_cert_content.perform(action);
                if edited {
                    self.keys_ui.import_form.certificate = self.keys_ui.import_cert_content.text();
                    self.keys_ui.import_form.cert_detected = false;
                    self.keys_ui.error = None;
                }
            }
            KeysMessage::BrowseCertFile => {
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(|| {
                        let file = rfd::FileDialog::new()
                            .set_title(crate::i18n::t("select_ssh_certificate_title"))
                            .add_filter("SSH certificate", &["pub"])
                            .pick_file();
                        match file {
                            Some(path) => std::fs::read_to_string(&path)
                                .map_err(|e| format!("Failed to read: {}", e)),
                            None => Err("cancelled".to_string()),
                        }
                    }),
                    |result| match result {
                        Ok(Ok(content)) => Message::Keys(KeysMessage::CertFileLoaded(content)),
                        Ok(Err(e)) => Message::Keys(KeysMessage::KeyFileBrowseError(e)),
                        Err(e) => Message::Keys(KeysMessage::KeyFileBrowseError(format!("Thread error: {}", e))),
                    },
                ));
            }
            KeysMessage::CertFileLoaded(content) => {
                self.keys_ui.import_form.certificate = content.trim().to_string();
                self.keys_ui.import_cert_content =
                    text_editor::Content::with_text(content.trim());
                // Explicitly picked, not auto-probed: no "detected" hint.
                self.keys_ui.import_form.cert_detected = false;
                self.keys_ui.error = None;
            }
            KeysMessage::ViewKeyCertificate(idx) => {
                if let Some(data) = self.build_cert_viewer(idx) {
                    self.cert_viewer = Some(data);
                }
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }
            KeysMessage::CloseCertViewer => {
                self.cert_viewer = None;
            }
            KeysMessage::RequestRemoveKeyCertificate(idx) => {
                if let Some(key) = self.keys.get(idx) {
                    let name = key.label.clone();
                    self.confirm_remove(name, Message::Keys(KeysMessage::RemoveKeyCertificate(idx)));
                }
                // The confirm dialog renders inside the main view (below the
                // cert-viewer overlay), so close the viewer first, otherwise
                // the confirmation would be hidden behind it.
                self.cert_viewer = None;
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }
            KeysMessage::RemoveKeyCertificate(idx) => {
                if let Some(key) = self.keys.get(idx) {
                    let mut key = key.clone();
                    key.certificate = None;
                    key.updated_at = chrono::Utc::now();
                    if let Some(vault) = &self.vault {
                        // `None` pem preserves the encrypted private blob and
                        // rewrites the (now empty) certificate column.
                        let _ = vault.save_key(&key, None);
                        self.load_data_from_vault();
                        self.keys_ui.success =
                            Some(crate::i18n::t("key_certificate_removed").into());
                    }
                }
                self.cert_viewer = None;
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }

            m => return Err(m),
        }
        Ok(Task::none())
    }

    /// Parse the certificate attached to key `idx` into a display-ready
    /// [`crate::state::CertViewerData`]. `None` when the key has no cert
    /// or the stored line no longer parses (defensive: it was validated
    /// at import, so this only guards against manual DB tampering).
    pub(crate) fn build_cert_viewer(
        &self,
        idx: usize,
    ) -> Option<crate::state::CertViewerData> {
        let key = self.keys.get(idx)?;
        let cert_line = key.certificate.as_deref()?;
        let cert = ssh_key::Certificate::from_openssh(cert_line.trim()).ok()?;

        // Unix-seconds bounds -> local wall-clock strings. `0` / u64::MAX
        // are OpenSSH's "unbounded" sentinels and render empty.
        let fmt_time = |secs: u64| -> String {
            if secs == 0 || secs == u64::MAX {
                return String::new();
            }
            match chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0) {
                Some(dt) => dt
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                None => String::new(),
            }
        };
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        let expired = cert.valid_before() != 0
            && cert.valid_before() != u64::MAX
            && now > cert.valid_before();
        let ca_fingerprint = cert
            .signature_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
            .to_string();

        Some(crate::state::CertViewerData {
            key_idx: idx,
            key_label: key.label.clone(),
            key_id: cert.key_id().to_string(),
            serial: cert.serial(),
            is_host: cert.cert_type() == ssh_key::certificate::CertType::Host,
            principals: cert.valid_principals().to_vec(),
            valid_from: fmt_time(cert.valid_after()),
            valid_until: fmt_time(cert.valid_before()),
            ca_fingerprint,
            expired,
        })
    }
}

/// Validate an attached certificate line against the key it belongs to
/// (B2). Returns the value to store (`None` = detach / no cert,
/// `Some(line)` = the trimmed cert) or an i18n error key. The check
/// mirrors the engine's `check_certificate` (same `public_key()` vs
/// key-data comparison) so the two layers can never disagree on the
/// same cert.
pub(super) fn validate_certificate(
    cert_input: &str,
    public_key_openssh: &str,
) -> Result<Option<String>, &'static str> {
    let trimmed = cert_input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // A pasted private key block is never a certificate; reject it so
    // secret material can never land in the plaintext cert column.
    if trimmed.contains("-----BEGIN") {
        return Err("cert_mismatch_error");
    }
    let cert = ssh_key::Certificate::from_openssh(trimmed)
        .map_err(|_| "cert_mismatch_error")?;
    // Host certificates authenticate servers, not users; reject.
    if cert.cert_type() == ssh_key::certificate::CertType::Host {
        return Err("cert_wrong_type_error");
    }
    let public = ssh_key::PublicKey::from_openssh(public_key_openssh)
        .map_err(|_| "cert_mismatch_error")?;
    if cert.public_key() != public.key_data() {
        return Err("cert_mismatch_error");
    }
    Ok(Some(trimmed.to_string()))
}

#[cfg(test)]
mod cert_validation_tests {
    use super::validate_certificate;
    use rand010 as rand;
    use ssh_key::{certificate, Algorithm, PrivateKey};

    /// A CA-signed certificate of `cert_type` for `user_key`, as its
    /// OpenSSH public line.
    fn make_cert(user_key: &PrivateKey, cert_type: certificate::CertType) -> String {
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let mut builder = certificate::Builder::new_with_random_nonce(
            &mut rand::rng(),
            user_key.public_key(),
            0,
            4_000_000_000,
        )
        .unwrap();
        builder.serial(7).unwrap();
        builder.key_id("id").unwrap();
        builder.cert_type(cert_type).unwrap();
        builder.valid_principal("tester").unwrap();
        builder.sign(&ca).unwrap().to_openssh().unwrap()
    }

    fn public_line(key: &PrivateKey) -> String {
        key.public_key().to_openssh().unwrap()
    }

    #[test]
    fn empty_input_detaches() {
        assert_eq!(validate_certificate("   ", "irrelevant"), Ok(None));
    }

    #[test]
    fn private_key_block_is_rejected() {
        // The secret-leak guard: BEGIN-block material must never be
        // accepted into the plaintext certificate column.
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(validate_certificate(pem, "irrelevant"), Err("cert_mismatch_error"));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(
            validate_certificate("not a certificate at all", "irrelevant"),
            Err("cert_mismatch_error")
        );
    }

    #[test]
    fn matching_user_cert_is_accepted() {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let cert = make_cert(&key, certificate::CertType::User);
        let stored = validate_certificate(&cert, &public_line(&key)).unwrap();
        assert_eq!(stored.as_deref(), Some(cert.trim()));
    }

    #[test]
    fn cert_for_another_key_is_rejected() {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let cert = make_cert(&other, certificate::CertType::User);
        assert_eq!(
            validate_certificate(&cert, &public_line(&key)),
            Err("cert_mismatch_error")
        );
    }

    #[test]
    fn host_cert_is_rejected() {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let cert = make_cert(&key, certificate::CertType::Host);
        assert_eq!(
            validate_certificate(&cert, &public_line(&key)),
            Err("cert_wrong_type_error")
        );
    }
}

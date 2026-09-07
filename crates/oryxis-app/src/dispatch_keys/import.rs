//! Key CRUD + the import panel, split out of `dispatch_keys`: panel
//! open/close, the import form fields, the private-key file dialog,
//! the `ImportKey` save path (public-only and private), edit / delete
//! / context menu, and the keychain searches. Called from
//! `handle_keys`.

#![allow(clippy::result_large_err)]

use iced::widget::text_editor;
use iced::Task;

use oryxis_vault::VaultError;

use super::certs::validate_certificate;
use crate::app::{KeysMessage, Message, Oryxis};
use crate::state::{OverlayContent, OverlayState, View};

impl Oryxis {
    pub(super) fn handle_keys_import(
        &mut self,
        message: KeysMessage,
    ) -> Result<Task<Message>, KeysMessage> {
        match message {
            // -- Keys --
            KeysMessage::ShowKeyPanel => {
                // Also navigate to the Keys screen, the import panel is rendered
                // inside view_keys(), so the user needs to be there to see it
                // (e.g. when they click "+ Key" from the host editor).
                self.active_view = View::Keys;
                self.active_tab = None;
                self.panels.key_panel = true;
                self.keys_ui.import_form.label.clear();
                self.keys_ui.import_content = text_editor::Content::new();
                self.keys_ui.import_form.pem.clear();
                self.keys_ui.import_form.passphrase.clear();
                self.keys_ui.import_form.passphrase_required = false;
                self.keys_ui.import_form.passphrase_visible = false;
                self.keys_ui.import_form.public_key.clear();
                self.keys_ui.import_form.certificate.clear();
                self.keys_ui.import_public_content = text_editor::Content::new();
                self.keys_ui.import_cert_content = text_editor::Content::new();
                self.keys_ui.import_form.cert_detected = false;
                self.keys_ui.error = None;
                self.keys_ui.success = None;
                self.keys_ui.import_form.editing_id = None;
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }
            KeysMessage::ShowKeyPanelPublicFocus => {
                // The ADD menu's "Import public key" entry (B3): the same
                // import panel, opened with the public-key field focused,
                // the security-key delegation flow (paste the sk- line,
                // no private material exists to import).
                let open = self.handle_keys(KeysMessage::ShowKeyPanel);
                return Ok(Task::batch([
                    open,
                    crate::widgets::focus_input(iced::widget::Id::new(
                        "panel-key-import-public",
                    )),
                ]));
            }
            KeysMessage::ShowKeyPanelCertFocus => {
                // The ADD menu's "Certificate" entry (B2.1): the same
                // import panel (a certificate lives on its key, one
                // entity), opened with the certificate field focused so
                // the intent lands somewhere visible.
                let open = self.handle_keys(KeysMessage::ShowKeyPanel);
                return Ok(Task::batch([
                    open,
                    crate::widgets::focus_input(iced::widget::Id::new(
                        "panel-key-import-cert",
                    )),
                ]));
            }
            KeysMessage::HideKeyPanel => {
                self.panels.key_panel = false;
                self.keys_ui.import_form.editing_id = None;
                self.keys_ui.import_form.passphrase.clear();
                self.keys_ui.import_form.passphrase_required = false;
                self.keys_ui.import_form.passphrase_visible = false;
                self.keys_ui.import_form.public_key.clear();
                self.keys_ui.import_form.certificate.clear();
                self.keys_ui.import_public_content = text_editor::Content::new();
                self.keys_ui.import_cert_content = text_editor::Content::new();
                self.keys_ui.import_form.cert_detected = false;
                // Errors raised inside the sidebar are scoped to it.
                // Closing the panel discards that context so the main
                // keychain area doesn't inherit a stale message.
                self.keys_ui.error = None;
                self.keys_ui.success = None;
            }
            KeysMessage::KeyImportLabelChanged(v) => self.keys_ui.import_form.label = v,
            KeysMessage::KeyContentAction(action) => {
                self.keys_ui.import_content.perform(action);
                let new_text = self.keys_ui.import_content.text();
                // Re-detect on every edit. If the user pastes an encrypted
                // PEM, the passphrase row should appear; if they swap to an
                // unencrypted one, it should hide. Clearing the cached
                // passphrase prevents leftover input from being applied
                // against a different key.
                if new_text != self.keys_ui.import_form.pem {
                    let encrypted = oryxis_vault::is_key_encrypted(&new_text);
                    if encrypted != self.keys_ui.import_form.passphrase_required {
                        self.keys_ui.import_form.passphrase.clear();
                    }
                    self.keys_ui.import_form.passphrase_required = encrypted;
                }
                self.keys_ui.import_form.pem = new_text;
            }
            KeysMessage::KeyImportPassphraseChanged(v) => {
                self.keys_ui.import_form.passphrase = v.into_inner();
                // Clear stale "wrong passphrase" feedback as the user types.
                self.keys_ui.error = None;
            }
            KeysMessage::KeyImportPassphraseToggleVisibility => {
                self.keys_ui.import_form.passphrase_visible = !self.keys_ui.import_form.passphrase_visible;
            }
            KeysMessage::BrowseKeyFile => {
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(|| {
                        let file = rfd::FileDialog::new()
                            .set_title(crate::i18n::t("select_private_key_title"))
                            .pick_file();
                        match file {
                            Some(path) => {
                                let filename = path
                                    .file_name()
                                    .map(|f| f.to_string_lossy().to_string())
                                    .unwrap_or_else(|| "imported-key".into());
                                let content = std::fs::read_to_string(&path)
                                    .map_err(|e| format!("Failed to read: {}", e))?;
                                // OpenSSH's implicit lookup: the public
                                // line sits next to the key as `<key>.pub`
                                // and a signed user cert as
                                // `<key>-cert.pub`. Auto-probe and prefill
                                // both when present, and only when they
                                // parse (a stray same-named file must not
                                // poison the field). The public line keeps
                                // the user's trailing comment, which
                                // deriving from the private key would lose.
                                let public = std::fs::read_to_string(
                                    format!("{}.pub", path.display()),
                                )
                                .ok()
                                .filter(|p| {
                                    ssh_key::PublicKey::from_openssh(p.trim()).is_ok()
                                });
                                let cert = std::fs::read_to_string(
                                    format!("{}-cert.pub", path.display()),
                                )
                                .ok()
                                .filter(|c| {
                                    ssh_key::Certificate::from_openssh(c.trim()).is_ok()
                                });
                                Ok((filename, content, public, cert))
                            }
                            None => Err("cancelled".to_string()),
                        }
                    }),
                    |result| match result {
                        Ok(Ok((filename, content, public, cert))) => {
                            Message::Keys(KeysMessage::KeyFileLoaded(filename, content, public, cert))
                        }
                        Ok(Err(e)) => Message::Keys(KeysMessage::KeyFileBrowseError(e)),
                        Err(e) => Message::Keys(KeysMessage::KeyFileBrowseError(format!("Thread error: {}", e))),
                    },
                ));
            }
            KeysMessage::KeyFileLoaded(filename, content, public, cert) => {
                if self.keys_ui.import_form.label.is_empty() {
                    self.keys_ui.import_form.label = filename;
                }
                self.keys_ui.import_content = text_editor::Content::with_text(&content);
                self.keys_ui.import_form.passphrase.clear();
                // Detect encryption now so the passphrase row appears as soon
                // as the file lands, not only after the user clicks Save.
                self.keys_ui.import_form.passphrase_required =
                    oryxis_vault::is_key_encrypted(&content);
                self.keys_ui.import_form.pem = content;
                // A sibling `<key>.pub` was found and parses: prefill the
                // editable public line (it carries the user's comment,
                // which deriving from the private key would lose).
                if let Some(public) = public {
                    self.keys_ui.import_form.public_key = public.trim().to_string();
                    self.keys_ui.import_public_content =
                        text_editor::Content::with_text(public.trim());
                }
                // A sibling `<key>-cert.pub` was found and parses: prefill
                // and flag the "certificate detected" hint.
                if let Some(cert) = cert {
                    self.keys_ui.import_form.certificate = cert.trim().to_string();
                    self.keys_ui.import_cert_content =
                        text_editor::Content::with_text(cert.trim());
                    self.keys_ui.import_form.cert_detected = true;
                }
                self.panels.key_panel = true;
                self.keys_ui.error = None;
                // The sidebar already shows "Loaded (X bytes)"; surfacing
                // a second toast in the main keychain area is just noise.
                self.keys_ui.success = None;
            }
            KeysMessage::KeyImportPublicAction(action) => {
                let edited = action.is_edit();
                self.keys_ui.import_public_content.perform(action);
                if edited {
                    self.keys_ui.import_form.public_key = self.keys_ui.import_public_content.text();
                    self.keys_ui.error = None;
                }
            }
            KeysMessage::KeyFileBrowseError(err) => {
                if !err.contains("cancelled") {
                    self.keys_ui.error = Some(err);
                }
            }
            KeysMessage::ImportKey => return Ok(self.import_key_from_form()),
            KeysMessage::RequestDeleteKey(idx) => {
                if let Some(key) = self.keys.get(idx) {
                    let name = key.label.clone();
                    self.confirm_remove(name, Message::Keys(KeysMessage::DeleteKey(idx)));
                }
            }
            KeysMessage::DeleteKey(idx) => {
                if let Some(key) = self.keys.get(idx) {
                    let id = key.id;
                    if let Some(vault) = &self.vault {
                        let _ = vault.delete_key(&id);
                        self.load_data_from_vault();
                        self.keys_ui.success = Some("Key deleted".into());
                    }
                }
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }
            KeysMessage::ShowKeyMenu(idx) => {
                if self.keys_ui.context_menu == Some(idx) {
                    self.keys_ui.context_menu = None;
                    self.overlay = None;
                } else {
                    self.keys_ui.context_menu = Some(idx);
                    let anchor = self.keynav_take_menu_anchor();
                    self.overlay = Some(OverlayState {
                        content: OverlayContent::KeyActions(idx),
                        x: anchor.0,
                        y: anchor.1,
                    });
                }
            }
            KeysMessage::HideKeyMenu => {
                self.keys_ui.context_menu = None;
                self.identity_context_menu = None;
                self.panels.keychain_add_menu = false;
                self.overlay = None;
            }
            KeysMessage::EditKey(idx) => {
                if let Some(key) = self.keys.get(idx) {
                    self.keys_ui.import_form.editing_id = Some(key.id);
                    self.keys_ui.import_form.label = key.label.clone();
                    // Load existing private key PEM from vault
                    let pem = self.vault.as_ref()
                        .and_then(|v| v.get_key_private(&key.id).ok().flatten())
                        .unwrap_or_default();
                    self.keys_ui.import_content = text_editor::Content::with_text(&pem);
                    self.keys_ui.import_form.pem = pem;
                    // Stored PEM is always unencrypted; no passphrase prompt.
                    self.keys_ui.import_form.passphrase.clear();
                    self.keys_ui.import_form.passphrase_required = false;
                    self.keys_ui.import_form.passphrase_visible = false;
                    self.keys_ui.import_form.public_key = key.public_key.clone();
                    self.keys_ui.import_public_content =
                        text_editor::Content::with_text(&key.public_key);
                    self.keys_ui.import_form.certificate =
                        key.certificate.clone().unwrap_or_default();
                    self.keys_ui.import_cert_content = text_editor::Content::with_text(
                        self.keys_ui.import_form.certificate.as_str(),
                    );
                    self.keys_ui.import_form.cert_detected = false;
                    self.panels.key_panel = true;
                    self.keys_ui.error = None;
                    self.keys_ui.success = None;
                    self.keys_ui.context_menu = None;
                    self.overlay = None;
                }
            }
            KeysMessage::KeySearchChanged(v) => {
                self.keys_ui.search = v;
            }
            KeysMessage::SnippetSearchChanged(v) => {
                self.snippet_search = v;
            }
            KeysMessage::HistorySearchChanged(v) => {
                self.history_search = v;
                // With the content toggle on, every edit re-arms the
                // debounced command/output search (see dispatch_history).
                if self.history_search_content {
                    return Ok(self.history_content_debounce());
                }
            }

            m => return Err(m),
        }
        Ok(Task::none())
    }

    /// The `ImportKey` save path: validate the form, import the key
    /// (public-only or private, with passphrase handling) and persist
    /// it. Extracted verbatim from the former inline match arm.
    fn import_key_from_form(&mut self) -> Task<Message> {
        let pem_empty = self.keys_ui.import_form.pem.trim().is_empty();
        if pem_empty && self.keys_ui.import_form.public_key.trim().is_empty() {
            self.keys_ui.error =
                Some(crate::i18n::t("key_select_file_first").into());
            return Task::none();
        }
        // Public-only import (B3): no private material at all, the
        // security-key / delegation path. The row persists with an
        // explicit NULL private column; edits of an existing row
        // preserve whatever the column holds (`None`), so clearing
        // the editor buffer can never silently drop a stored
        // private key.
        if pem_empty {
            let label = if self.keys_ui.import_form.label.is_empty() {
                "imported-key".to_string()
            } else {
                self.keys_ui.import_form.label.clone()
            };
            let input = self.keys_ui.import_form.public_key.trim().to_string();
            if input.contains("-----BEGIN") {
                self.keys_ui.error =
                    Some(crate::i18n::t("public_key_only_error").to_string());
                return Task::none();
            }
            let mut key = match oryxis_vault::import_public_key(&label, &input) {
                Ok(key) => key,
                Err(_) => {
                    self.keys_ui.error = Some(
                        crate::i18n::t("public_key_invalid_error").to_string(),
                    );
                    return Task::none();
                }
            };
            let editing = self.keys_ui.import_form.editing_id.is_some();
            if let Some(existing_id) = self.keys_ui.import_form.editing_id {
                key.id = existing_id;
                if let Some(existing) =
                    self.keys.iter().find(|k| k.id == existing_id)
                {
                    key.expose_via_agent = existing.expose_via_agent;
                    key.created_at = existing.created_at;
                }
                // Editing an existing row keeps its private column
                // (`private = None` below). If that column holds a
                // real private key, the public line the user typed
                // MUST still certify it: otherwise the row would
                // pair an old private with a new, mismatched public
                // and the agent would advertise a blob it signs for
                // with the wrong key. A public-only row (NULL
                // private) has nothing to check against, so this
                // only fires when a private is actually stored.
                let stored_private = self
                    .vault
                    .as_ref()
                    .and_then(|v| v.get_key_private(&existing_id).ok().flatten());
                if let Some(pem) = stored_private {
                    match oryxis_vault::import_key("_check", &pem, None) {
                        Ok(generated) => {
                            match validate_public_key(&input, &generated.key.public_key) {
                                Ok(_) => {}
                                Err(err_key) => {
                                    self.keys_ui.error =
                                        Some(crate::i18n::t(err_key).to_string());
                                    return Task::none();
                                }
                            }
                            // Carry the stored key's algorithm so a
                            // public-line edit can't relabel an RSA
                            // 2048/3072 row as 4096 (public lines
                            // don't carry the modulus size).
                            key.algorithm = generated.key.algorithm;
                        }
                        // Stored private unreadable (corrupt / locked):
                        // fail closed rather than save a possibly
                        // mismatched pair.
                        Err(_) => {
                            self.keys_ui.error = Some(
                                crate::i18n::t("public_key_mismatch_error")
                                    .to_string(),
                            );
                            return Task::none();
                        }
                    }
                }
            }
            // The certificate field wins over a cert embedded in
            // the public line (same validation as the private
            // path); empty keeps whatever the line carried.
            match validate_certificate(
                &self.keys_ui.import_form.certificate,
                &key.public_key,
            ) {
                Ok(Some(cert)) => key.certificate = Some(cert),
                Ok(None) => {}
                Err(err_key) => {
                    self.keys_ui.error =
                        Some(crate::i18n::t(err_key).to_string());
                    return Task::none();
                }
            }
            if let Some(vault) = &self.vault {
                let private = if editing { None } else { Some("") };
                match vault.save_key(&key, private) {
                    Ok(()) => {
                        let verb = if editing { "updated" } else { "imported" };
                        self.keys_ui.error = None;
                        self.keys_ui.success =
                            Some(format!("Key '{}' {}", label, verb));
                        self.keys_ui.import_form = crate::state::KeyImportForm::default();
                        self.keys_ui.import_content = text_editor::Content::new();
                        self.keys_ui.import_public_content =
                            text_editor::Content::new();
                        self.keys_ui.import_cert_content =
                            text_editor::Content::new();
                        self.panels.key_panel = false;
                        self.load_data_from_vault();
                    }
                    Err(e) => self.keys_ui.error = Some(e.to_string()),
                }
            }
            return Task::none();
        }
        // If we already know the key is encrypted but the user
        // clicked Save with an empty passphrase, give explicit
        // feedback instead of silently leaving the row visible.
        if self.keys_ui.import_form.passphrase_required && self.keys_ui.import_form.passphrase.is_empty() {
            self.keys_ui.error =
                Some(crate::i18n::t("key_passphrase_required_msg").to_string());
            return Task::none();
        }
        let label = if self.keys_ui.import_form.label.is_empty() {
            "imported-key".to_string()
        } else {
            self.keys_ui.import_form.label.clone()
        };
        let pass_opt = if self.keys_ui.import_form.passphrase.is_empty() {
            None
        } else {
            Some(self.keys_ui.import_form.passphrase.as_str())
        };
        match oryxis_vault::import_key(&label, &self.keys_ui.import_form.pem, pass_opt) {
            Ok(mut generated) => {
                // If editing an existing key, preserve the fields
                // that live outside the import form. `import_key`
                // rebuilds a fresh `SshKey` (expose_via_agent = true,
                // created_at = now), so re-saving after an edit would
                // silently re-arm a key the user had removed from the
                // ssh-agent and reset its creation date (breaking the
                // by-date sort). Carry the id and both fields over.
                if let Some(existing_id) = self.keys_ui.import_form.editing_id {
                    generated.key.id = existing_id;
                    if let Some(existing) =
                        self.keys.iter().find(|k| k.id == existing_id)
                    {
                        generated.key.expose_via_agent = existing.expose_via_agent;
                        generated.key.created_at = existing.created_at;
                    }
                }
                // Apply the editable public line (B2.1). Empty keeps
                // the derived one; non-empty must parse and carry the
                // private key's key data (a different comment is
                // fine, that is the point of the field).
                match validate_public_key(
                    &self.keys_ui.import_form.public_key,
                    &generated.key.public_key,
                ) {
                    Ok(Some(public)) => generated.key.public_key = public,
                    Ok(None) => {}
                    Err(key) => {
                        self.keys_ui.error = Some(crate::i18n::t(key).to_string());
                        return Task::none();
                    }
                }
                // Validate + attach the certificate. A mismatch or a
                // host cert is an inline error, never a silent save
                // (the engine's `check_certificate` is the belt to
                // this brace, but the editor stops it here first).
                match validate_certificate(
                    &self.keys_ui.import_form.certificate,
                    &generated.key.public_key,
                ) {
                    Ok(cert) => generated.key.certificate = cert,
                    Err(key) => {
                        self.keys_ui.error = Some(crate::i18n::t(key).to_string());
                        return Task::none();
                    }
                }
                if let Some(vault) = &self.vault {
                    match vault.save_key(&generated.key, Some(&generated.private_pem)) {
                        Ok(()) => {
                            let verb = if self.keys_ui.import_form.editing_id.is_some() { "updated" } else { "imported" };
                            self.keys_ui.error = None;
                            self.keys_ui.success = Some(format!("Key '{}' {}", label, verb));
                            self.keys_ui.import_form.label.clear();
                            self.keys_ui.import_content = text_editor::Content::new();
                            self.keys_ui.import_form.pem.clear();
                            self.keys_ui.import_form.passphrase.clear();
                            self.keys_ui.import_form.passphrase_required = false;
                            self.keys_ui.import_form.passphrase_visible = false;
                            self.keys_ui.import_form.public_key.clear();
                            self.keys_ui.import_form.certificate.clear();
                            self.keys_ui.import_public_content =
                                text_editor::Content::new();
                            self.keys_ui.import_cert_content =
                                text_editor::Content::new();
                            self.keys_ui.import_form.cert_detected = false;
                            self.panels.key_panel = false;
                            self.keys_ui.import_form.editing_id = None;
                            self.load_data_from_vault();
                        }
                        Err(e) => self.keys_ui.error = Some(e.to_string()),
                    }
                }
            }
            Err(VaultError::KeyNeedsPassphrase) => {
                self.keys_ui.import_form.passphrase_required = true;
                self.keys_ui.error = None;
            }
            Err(VaultError::WrongKeyPassphrase) => {
                self.keys_ui.import_form.passphrase_required = true;
                self.keys_ui.error = Some(crate::i18n::t("key_passphrase_wrong").to_string());
            }
            Err(VaultError::UnsupportedKeyKind(kind)) => {
                self.keys_ui.error = Some(
                    crate::i18n::t("key_unsupported_kind").replace("{kind}", &kind),
                );
            }
            Err(e) => self.keys_ui.error = Some(format!("Import failed: {}", e)),
        }
        Task::none()
    }
}

/// Validate the editable public-key line against the public key derived
/// from the private key (B2.1). Returns `Ok(None)` when the field is
/// empty (keep the derived line), `Ok(Some(line))` when the input parses
/// and carries the same key data (a different trailing comment is fine,
/// preserving it is the point of the field), or an i18n error key.
fn validate_public_key(
    public_input: &str,
    derived_openssh: &str,
) -> Result<Option<String>, &'static str> {
    let trimmed = public_input.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    // A pasted private key block is never a public line; reject it so
    // secret material can never land in the plaintext public column.
    if trimmed.contains("-----BEGIN") {
        return Err("public_key_invalid_error");
    }
    let public = ssh_key::PublicKey::from_openssh(trimmed)
        .map_err(|_| "public_key_invalid_error")?;
    let derived = ssh_key::PublicKey::from_openssh(derived_openssh)
        .map_err(|_| "public_key_invalid_error")?;
    if public.key_data() != derived.key_data() {
        return Err("public_key_mismatch_error");
    }
    Ok(Some(trimmed.to_string()))
}

#[cfg(test)]
mod public_key_validation_tests {
    use super::validate_public_key;
    use rand010 as rand;
    use ssh_key::{Algorithm, PrivateKey};

    fn public_line(key: &PrivateKey) -> String {
        key.public_key().to_openssh().unwrap()
    }

    #[test]
    fn empty_input_keeps_the_derived_line() {
        assert_eq!(validate_public_key("   ", "irrelevant"), Ok(None));
    }

    #[test]
    fn private_key_block_is_rejected() {
        // The secret-leak guard: BEGIN-block material must never be
        // accepted into the plaintext public-key column.
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----";
        assert_eq!(validate_public_key(pem, "irrelevant"), Err("public_key_invalid_error"));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(
            validate_public_key("not a public key", "irrelevant"),
            Err("public_key_invalid_error")
        );
    }

    #[test]
    fn matching_line_with_custom_comment_is_kept() {
        // Editing the trailing comment is the field's use case (the
        // comparison is on key data, not the string).
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let derived = public_line(&key);
        let blob = derived.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
        let edited = format!("{blob} wilson@workstation");
        assert_eq!(
            validate_public_key(&edited, &derived),
            Ok(Some(edited.clone()))
        );
    }

    #[test]
    fn another_keys_public_line_is_rejected() {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        assert_eq!(
            validate_public_key(&public_line(&other), &public_line(&key)),
            Err("public_key_mismatch_error")
        );
    }
}

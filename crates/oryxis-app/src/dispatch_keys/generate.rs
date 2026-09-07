//! The keygen panel flow (keychain > ADD > Generate key), split out
//! of `dispatch_keys`: panel open/close, form fields, the blocking
//! generation task, and the public/private export actions. Called
//! from `handle_keys`.

#![allow(clippy::result_large_err)]

use iced::Task;

use crate::app::{KeysMessage, Message, Oryxis};
use crate::state::View;

impl Oryxis {
    pub(super) fn handle_keys_generate(
        &mut self,
        message: KeysMessage,
    ) -> Result<Task<Message>, KeysMessage> {
        match message {
            // -- Key generation (keychain > ADD > Generate key) --
            KeysMessage::ShowKeyGeneratePanel => {
                self.active_view = View::Keys;
                self.active_tab = None;
                // Mutually exclusive with the import/identity panels.
                self.panels.key_panel = false;
                self.panels.identity_panel = false;
                self.panels.key_generate_panel = true;
                self.keys_ui.generate_form = crate::state::KeyGenerateForm::default();
                self.keys_ui.error = None;
                self.keys_ui.success = None;
                self.keys_ui.context_menu = None;
                self.overlay = None;
            }
            KeysMessage::HideKeyGeneratePanel => {
                self.panels.key_generate_panel = false;
                self.keys_ui.generate_form = crate::state::KeyGenerateForm::default();
            }
            KeysMessage::KeyGenLabelChanged(v) => {
                self.keys_ui.generate_form.label = v;
                self.keys_ui.generate_form.error = None;
            }
            KeysMessage::KeyGenCommentChanged(v) => self.keys_ui.generate_form.comment = v,
            KeysMessage::KeyGenAlgoSelected(a) => self.keys_ui.generate_form.algo = a,
            KeysMessage::KeyGenBitsSelected(b) => self.keys_ui.generate_form.rsa_bits = b,
            KeysMessage::KeyGenCurveSelected(c) => self.keys_ui.generate_form.ecdsa_curve = c,
            KeysMessage::GenerateKey => {
                if self.keys_ui.generate_form.working {
                    return Ok(Task::none());
                }
                let label = self.keys_ui.generate_form.label.trim().to_string();
                if label.is_empty() {
                    self.keys_ui.generate_form.error =
                        Some(crate::i18n::t("keygen_label_required").to_string());
                    return Ok(Task::none());
                }
                let comment = self.keys_ui.generate_form.comment.trim().to_string();
                let spec = match self.keys_ui.generate_form.algo {
                    crate::state::KeyGenAlgo::Ed25519 => oryxis_vault::GenerateSpec::Ed25519,
                    crate::state::KeyGenAlgo::Rsa => oryxis_vault::GenerateSpec::Rsa {
                        bits: self.keys_ui.generate_form.rsa_bits,
                    },
                    crate::state::KeyGenAlgo::Ecdsa => oryxis_vault::GenerateSpec::Ecdsa {
                        curve: self.keys_ui.generate_form.ecdsa_curve,
                    },
                };
                self.keys_ui.generate_form.working = true;
                self.keys_ui.generate_form.error = None;
                // RSA 4096 takes seconds: run on the blocking pool so
                // the UI keeps painting the spinner.
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(move || {
                        oryxis_vault::generate_key(&label, &comment, spec)
                            .map(std::sync::Arc::new)
                            .map_err(|e| e.to_string())
                    }),
                    |result| match result {
                        Ok(inner) => Message::Keys(KeysMessage::KeyGenerated(inner)),
                        Err(e) => Message::Keys(KeysMessage::KeyGenerated(Err(format!("Thread error: {}", e)))),
                    },
                ));
            }
            KeysMessage::KeyGenerated(result) => {
                self.keys_ui.generate_form.working = false;
                match result {
                    Ok(generated) => {
                        // A soft auto-lock may have landed while the task
                        // ran; a locked vault cannot encrypt the private
                        // material, so the generated key is dropped (the
                        // panel was already swept by the lock sweep).
                        if self.vault_ui.state != crate::state::VaultState::Unlocked {
                            return Ok(Task::none());
                        }
                        let Some(vault) = &self.vault else {
                            return Ok(Task::none());
                        };
                        if let Err(e) =
                            vault.save_key(&generated.key, Some(&generated.private_pem))
                        {
                            self.keys_ui.generate_form.error = Some(e.to_string());
                            return Ok(Task::none());
                        }
                        self.keys = vault.list_keys().unwrap_or_default();
                        self.keys_ui.generate_form.result =
                            Some(crate::state::GeneratedKeyView {
                                id: generated.key.id,
                                label: generated.key.label.clone(),
                                fingerprint: generated.key.fingerprint.clone(),
                                public_key: generated.key.public_key.clone(),
                            });
                    }
                    Err(e) => self.keys_ui.generate_form.error = Some(e),
                }
            }
            KeysMessage::CopyGeneratedPublicKey => {
                if let Some(result) = &self.keys_ui.generate_form.result {
                    return Ok(iced::clipboard::write(result.public_key.clone()).discard());
                }
            }
            KeysMessage::SaveGeneratedPublicKeyFile => {
                let Some(result) = self.keys_ui.generate_form.result.clone() else {
                    return Ok(Task::none());
                };
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(move || {
                        let file = rfd::FileDialog::new()
                            .set_title(crate::i18n::t("save_public_key_title"))
                            .set_file_name(format!("{}.pub", sanitize_key_filename(&result.label)))
                            .save_file();
                        match file {
                            Some(path) => std::fs::write(&path, format!("{}\n", result.public_key))
                                .map_err(|e| format!("Failed to write: {}", e)),
                            None => Err("cancelled".to_string()),
                        }
                    }),
                    |result| match result {
                        Ok(Ok(())) => Message::Keys(KeysMessage::KeyFileBrowseError(String::new())),
                        Ok(Err(e)) => Message::Keys(KeysMessage::KeyFileBrowseError(e)),
                        Err(e) => Message::Keys(KeysMessage::KeyFileBrowseError(format!("Thread error: {}", e))),
                    },
                ));
            }
            KeysMessage::KeyGenExportPassphraseChanged(v) => {
                self.keys_ui.generate_form.export_passphrase = v.into_inner();
                self.keys_ui.generate_form.error = None;
            }
            KeysMessage::KeyGenExportPassphraseConfirmChanged(v) => {
                self.keys_ui.generate_form.export_passphrase_confirm = v.into_inner();
                self.keys_ui.generate_form.error = None;
            }
            KeysMessage::KeyGenExportPassphraseToggleVisibility => {
                self.keys_ui.generate_form.export_passphrase_visible =
                    !self.keys_ui.generate_form.export_passphrase_visible;
            }
            KeysMessage::KeyGenExportPassphraseConfirmToggleVisibility => {
                self.keys_ui.generate_form.export_passphrase_confirm_visible =
                    !self.keys_ui.generate_form.export_passphrase_confirm_visible;
            }
            KeysMessage::ExportGeneratedPrivateKey => {
                let Some(result) = self.keys_ui.generate_form.result.clone() else {
                    return Ok(Task::none());
                };
                let pass = self.keys_ui.generate_form.export_passphrase.clone();
                let confirm = self.keys_ui.generate_form.export_passphrase_confirm.clone();
                if pass != confirm {
                    self.keys_ui.generate_form.error =
                        Some(crate::i18n::t("keygen_passphrase_mismatch").to_string());
                    return Ok(Task::none());
                }
                // The PEM is re-read from the vault (never held in form
                // state) and passphrase-encrypted here when one was
                // given; an empty pair exports plaintext (the panel
                // shows an explicit warning line for that case).
                let pem = match self.vault.as_ref().map(|v| v.get_key_private(&result.id)) {
                    Some(Ok(Some(pem))) => pem,
                    Some(Ok(None)) | None => {
                        self.keys_ui.generate_form.error =
                            Some(crate::i18n::t("key_not_found").into());
                        return Ok(Task::none());
                    }
                    Some(Err(e)) => {
                        self.keys_ui.generate_form.error = Some(e.to_string());
                        return Ok(Task::none());
                    }
                };
                let payload = if pass.is_empty() {
                    pem
                } else {
                    match oryxis_vault::encrypt_private_pem(&pem, &pass) {
                        Ok(enc) => enc,
                        Err(e) => {
                            self.keys_ui.generate_form.error = Some(e.to_string());
                            return Ok(Task::none());
                        }
                    }
                };
                let name = sanitize_key_filename(&result.label);
                return Ok(Task::perform(
                    tokio::task::spawn_blocking(move || {
                        let file = rfd::FileDialog::new()
                            .set_title(crate::i18n::t("keygen_export_private"))
                            .set_file_name(name)
                            .save_file();
                        match file {
                            Some(path) => {
                                write_private_key_file(&path, &payload)
                                    .map_err(|e| format!("Failed to write: {}", e))
                            }
                            None => Err("cancelled".to_string()),
                        }
                    }),
                    |result| match result {
                        Ok(Ok(())) => Message::Keys(KeysMessage::KeyFileBrowseError(String::new())),
                        Ok(Err(e)) => Message::Keys(KeysMessage::KeyFileBrowseError(e)),
                        Err(e) => Message::Keys(KeysMessage::KeyFileBrowseError(format!("Thread error: {}", e))),
                    },
                ));
            }

            m => return Err(m),
        }
        Ok(Task::none())
    }
}

/// A filesystem-safe file stem from a key label.
fn sanitize_key_filename(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '-' })
        .collect();
    if cleaned.is_empty() { "oryxis-key".into() } else { cleaned }
}

/// Write exported private-key material with owner-only permissions on
/// Unix (0600, matching ssh-keygen); Windows relies on the user
/// profile ACLs like every SSH tool there.
fn write_private_key_file(path: &std::path::Path, payload: &str) -> std::io::Result<()> {
    std::fs::write(path, payload)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

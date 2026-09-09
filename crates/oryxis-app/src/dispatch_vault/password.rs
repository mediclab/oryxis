//! Master-password management from inside an unlocked vault: set one
//! on a vault that has none, change the current one, or remove it.
//!
//! Removing is destructive, so the switch arms a confirm instead of
//! acting. Setting and changing both run the Argon2id calibration
//! off-thread first and land back here as `VaultKdfCalibrated`.

use super::*;

impl Oryxis {
    pub(super) fn handle_vault_password(
        &mut self,
        message: VaultMessage,
    ) -> Task<Message> {
        match message {

            // ── Vault password management ──
            VaultMessage::ToggleVaultPassword => {
                if self.vault_ui.has_user_password {
                    // Removing encryption is destructive: arm the confirm
                    // prompt instead of dropping the password on a single
                    // click. The switch stays on until the user confirms.
                    // Close the change form so the two can't stack.
                    self.vault_ui.confirm_remove_password = true;
                    self.vault_ui.change_password_open = false;
                    self.vault_ui.password_error = None;
                } else {
                    // No password yet: the switch reveals / hides the inline
                    // set-password form. Nothing is committed until the user
                    // types, confirms, and presses Set Password.
                    self.vault_ui.show_password_form = !self.vault_ui.show_password_form;
                    // Offer the biometric opt-in pre-checked whenever the
                    // platform can service it (password creation is the
                    // natural moment to enable the convenience layer).
                    self.vault_ui.setup_enable_biometric = self.biometric_available;
                    self.vault_ui.new_password.clear();
                    self.vault_ui.confirm_password.clear();
                    self.vault_ui.password_error = None;
                    // Hiding the form is a cancel: abort any calibration
                    // that is still in flight (see `pending_kdf_pw`).
                    self.vault_ui.pending_kdf_pw = None;
                }
            }
            VaultMessage::ConfirmRemoveVaultPassword => {
                if let Some(vault) = &mut self.vault {
                    match vault.remove_user_password() {
                        Ok(()) => {
                            self.vault_ui.has_user_password = false;
                            self.vault_ui.show_password_form = false;
                            self.vault_ui.confirm_remove_password = false;
                            self.vault_ui.password_error = None;
                            self.vault_ui.new_password.clear();
                            self.vault_ui.confirm_password.clear();
                            // A passwordless vault has nothing to gate, so
                            // drop any biometric enrollment and turn the
                            // setting off (it would otherwise dangle on).
                            if self.prefs.biometric_unlock_enabled {
                                self.biometric_forget();
                                self.prefs.biometric_unlock_enabled = false;
                                self.persist_setting("biometric_unlock_enabled", "false");
                            }
                        }
                        Err(e) => {
                            self.vault_ui.password_error = Some(e.to_string());
                        }
                    }
                }
            }
            VaultMessage::CancelRemoveVaultPassword => {
                self.vault_ui.confirm_remove_password = false;
                self.vault_ui.password_error = None;
            }
            VaultMessage::VaultNewPasswordChanged(pw) => {
                self.vault_ui.new_password = pw.into_inner();
            }
            VaultMessage::VaultConfirmPasswordChanged(pw) => {
                self.vault_ui.confirm_password = pw.into_inner();
            }
            VaultMessage::SetVaultPassword => {
                if self.vault_ui.new_password.len() < 4 {
                    self.vault_ui.password_error =
                        Some(crate::i18n::t("password_too_short").to_string());
                    return Task::none();
                }
                // Both fields are hidden, so a typo would otherwise be
                // invisible until the next unlock (when it's too late).
                if self.vault_ui.new_password != self.vault_ui.confirm_password {
                    self.vault_ui.password_error =
                        Some(crate::i18n::t("passwords_do_not_match").to_string());
                    return Task::none();
                }
                // Phase 1 (E1): calibrate off-thread, apply on callback.
                if self.vault_ui.calibrating {
                    return Task::none();
                }
                self.vault_ui.calibrating = true;
                self.vault_ui.password_error = None;
                self.vault_ui.pending_kdf_pw = Some(self.vault_ui.new_password.clone());
                return calibrate_kdf_task(crate::state::VaultPwOp::SetUser);
            }
            VaultMessage::OpenChangeVaultPassword => {
                // Reveal the change form; start from a clean slate so a
                // stale value from a previous open can't leak in. Dismiss
                // any armed remove-confirm so the two can't stack.
                self.vault_ui.change_password_open = true;
                self.vault_ui.confirm_remove_password = false;
                self.vault_ui.current_password.clear();
                self.vault_ui.new_password.clear();
                self.vault_ui.confirm_password.clear();
                self.vault_ui.password_error = None;
            }
            VaultMessage::CancelChangeVaultPassword => {
                self.vault_ui.change_password_open = false;
                self.vault_ui.current_password.clear();
                self.vault_ui.new_password.clear();
                self.vault_ui.confirm_password.clear();
                self.vault_ui.password_error = None;
                // Cancel during the ~1s calibration: dropping the snapshot
                // aborts the pending apply, so the rotation can't land after
                // the user backed out.
                self.vault_ui.pending_kdf_pw = None;
            }
            VaultMessage::VaultCurrentPasswordChanged(pw) => {
                self.vault_ui.current_password = pw.into_inner();
            }
            VaultMessage::ConfirmChangeVaultPassword => {
                if self.vault_ui.new_password.len() < 4 {
                    self.vault_ui.password_error =
                        Some(crate::i18n::t("password_too_short").to_string());
                    return Task::none();
                }
                if self.vault_ui.new_password != self.vault_ui.confirm_password {
                    self.vault_ui.password_error =
                        Some(crate::i18n::t("passwords_do_not_match").to_string());
                    return Task::none();
                }
                if self.vault_ui.calibrating {
                    return Task::none();
                }
                // Verify the current password BEFORE the (slow) calibration:
                // no reason to calibrate for a change that will be rejected.
                // The vault is already unlocked, so this guards against
                // someone changing the password at an unattended session and
                // against a typo silently rotating to an unknown key.
                if let Some(vault) = &self.vault {
                    match vault.verify_password(&self.vault_ui.current_password) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.vault_ui.password_error = Some(
                                crate::i18n::t("current_password_incorrect").to_string(),
                            );
                            return Task::none();
                        }
                        Err(e) => {
                            self.vault_ui.password_error = Some(e.to_string());
                            return Task::none();
                        }
                    }
                }
                // Phase 1 (E1): current verified, calibrate off-thread.
                self.vault_ui.calibrating = true;
                self.vault_ui.password_error = None;
                self.vault_ui.pending_kdf_pw = Some(self.vault_ui.new_password.clone());
                return calibrate_kdf_task(crate::state::VaultPwOp::Change);
            }
            VaultMessage::VaultKdfCalibrated(op, params) => {
                // Phase 2 (E1): apply the pending set / change-password with
                // the tuned KDF params. The ~1s derive here runs on the UI
                // thread, same cost as an unlock (the plan accepts that);
                // only the multi-probe calibration went off-thread.
                self.vault_ui.calibrating = false;
                // The password to apply is the snapshot taken when the user
                // confirmed, never the live buffers (they may have been
                // edited or cleared during the calibration). A missing
                // snapshot means the flow was cancelled: discard the result.
                let Some(pw) = self.vault_ui.pending_kdf_pw.take() else {
                    return Task::none();
                };
                let Some(vault) = &mut self.vault else {
                    return Task::none();
                };
                match op {
                    crate::state::VaultPwOp::FirstSetup => {
                        match vault.set_master_password_with_params(&pw, params) {
                            Ok(()) => {
                                let _ = vault.set_setting("has_user_password", "1");
                                self.vault_ui.has_user_password = true;
                                self.vault_ui.state = VaultState::Unlocked;
                                self.vault_ui.error = None;
                                self.master_password = Some(pw.clone());
                                let bio_task = self
                                    .biometric_setup_enroll(&pw)
                                    .unwrap_or_else(Task::none);
                                self.vault_ui.password_input.clear();
                                self.vault_ui.password_visible = false;
                                self.load_data_from_vault();
                                // The app's own fetches, deferred from
                                // boot until the onboarding had its say
                                // on offline mode.
                                let fetches = Task::batch(
                                    self.boot_fetch_tasks()
                                        .into_iter()
                                        .chain(self.unlock_fetch_tasks()),
                                );
                                return Task::batch([
                                    bio_task,
                                    fetches,
                                    self.agent_boot_task(),
                                    self.take_perf_mode_toast_task(),
                                    // Onboarding's import offer, now
                                    // that there IS a vault to import
                                    // into.
                                    self.take_onboarding_import_task(),
                                    crate::widgets::focus_input(iced::widget::Id::new(
                                        "search-dashboard",
                                    )),
                                ]);
                            }
                            Err(e) => self.vault_ui.error = Some(e.to_string()),
                        }
                    }
                    crate::state::VaultPwOp::SetUser => {
                        match vault.set_user_password_with_params(&pw, params) {
                            Ok(()) => {
                                self.vault_ui.has_user_password = true;
                                self.vault_ui.show_password_form = false;
                                self.vault_ui.password_error = None;
                                self.master_password = Some(pw.clone());
                                let bio_task = self.biometric_setup_enroll(&pw);
                                self.vault_ui.new_password.clear();
                                self.vault_ui.confirm_password.clear();
                                if let Some(toast) = bio_task {
                                    return toast;
                                }
                            }
                            Err(e) => self.vault_ui.password_error = Some(e.to_string()),
                        }
                    }
                    crate::state::VaultPwOp::Change => {
                        match vault.set_user_password_with_params(&pw, params) {
                            Ok(()) => {
                                self.vault_ui.change_password_open = false;
                                self.vault_ui.password_error = None;
                                self.master_password = Some(pw.clone());
                                self.biometric_reenroll(&pw);
                                self.vault_ui.current_password.clear();
                                self.vault_ui.new_password.clear();
                                self.vault_ui.confirm_password.clear();
                                return self.show_toast(
                                    crate::i18n::t("password_updated").to_string(),
                                );
                            }
                            Err(e) => self.vault_ui.password_error = Some(e.to_string()),
                        }
                    }
                }
            }
            // The parent routed us here, so anything else is a
            // grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}

//! Getting into the vault: the first-run setup screen and the lock
//! screen. Create it with a master password, create it without one,
//! reset a vault whose password is lost, or unlock an existing one.
//!
//! Every arm here ends with an open vault or an error on the same
//! screen; nothing in this file runs while the vault is unlocked.

use super::*;

impl Oryxis {
    pub(super) fn handle_vault_open(
        &mut self,
        message: VaultMessage,
    ) -> Task<Message> {
        match message {
            // -- Vault --
            VaultMessage::VaultPasswordChanged(pw) => {
                self.vault_ui.password_input = pw.into_inner();
            }
            VaultMessage::VaultTogglePasswordVisibility => {
                self.vault_ui.password_visible = !self.vault_ui.password_visible;
            }
            VaultMessage::VaultSetup => {
                if self.vault_ui.password_input.len() < 4 {
                    self.vault_ui.error =
                        Some(crate::i18n::t("password_too_short").to_string());
                    return Task::none();
                }
                // Phase 1 (E1): calibrate Argon2id off the UI thread, then
                // apply on `VaultKdfCalibrated`. A doubled click while the
                // spinner is up is a no-op.
                if self.vault_ui.calibrating {
                    return Task::none();
                }
                self.vault_ui.calibrating = true;
                self.vault_ui.error = None;
                // Snapshot the confirmed password: the input stays live
                // during the calibration and must not be re-read at apply
                // time (see `pending_kdf_pw`).
                self.vault_ui.pending_kdf_pw = Some(self.vault_ui.password_input.clone());
                return calibrate_kdf_task(crate::state::VaultPwOp::FirstSetup);
            }
            VaultMessage::VaultSkipPassword => {
                if let Some(vault) = &mut self.vault {
                    match vault.open_without_password() {
                        Ok(()) => {
                            self.vault_ui.state = VaultState::Unlocked;
                            self.vault_ui.error = None;
                            self.load_data_from_vault();
                            // The app's own fetches, deferred from boot
                            // until the onboarding had its say on
                            // offline mode.
                            let fetches = Task::batch(
                                self.boot_fetch_tasks()
                                    .into_iter()
                                    .chain(self.unlock_fetch_tasks()),
                            );
                            return Task::batch([
                                self.agent_boot_task(),
                                self.take_perf_mode_toast_task(),
                                // Onboarding's import offer, now that
                                // there IS a vault to import into.
                                self.take_onboarding_import_task(),
                                fetches,
                                crate::widgets::focus_input(iced::widget::Id::new(
                                    "search-dashboard",
                                )),
                            ]);
                        }
                        Err(VaultError::InvalidPassword) => {
                            self.vault_ui.error = Some(
                                crate::i18n::t("vault_already_has_password").to_string(),
                            );
                        }
                        Err(e) => {
                            self.vault_ui.error = Some(format!("Failed to create vault: {}", e));
                        }
                    }
                }
            }
            VaultMessage::VaultDestroyConfirm => {
                self.vault_ui.destroy_confirm = !self.vault_ui.destroy_confirm;
            }
            VaultMessage::VaultDestroy => {
                if let Some(vault) = &mut self.vault {
                    match vault.destroy_and_recreate() {
                        Ok(()) => {
                            self.vault_ui.state = VaultState::NeedSetup;
                            self.vault_ui.error = None;
                            self.vault_ui.destroy_confirm = false;
                            self.vault_ui.password_input.clear();
                            self.vault_ui.password_visible = false;
                        }
                        Err(e) => {
                            self.vault_ui.error = Some(format!("Failed to reset vault: {}", e));
                        }
                    }
                }
            }
            VaultMessage::VaultUnlock => {
                // Ignore the submit when no password was typed (pressing
                // Enter on an empty field or clicking Unlock with it blank
                // shouldn't run a doomed unlock attempt or surface an error).
                if self.vault_ui.password_input.is_empty() {
                    return Task::none();
                }
                if let Some(vault) = &mut self.vault {
                    match vault.unlock(&self.vault_ui.password_input) {
                        Ok(()) => {
                            self.vault_ui.state = VaultState::Unlocked;
                            self.vault_ui.error = None;
                            // Stamp the unlock so the key router can swallow
                            // the Enter that submitted the password (it
                            // arrives one message later, post-unlock; see
                            // `Oryxis::last_unlock`).
                            self.last_unlock = Some(std::time::Instant::now());
                            // Retain the password in memory so we can spawn
                            // child windows with it via stdin pipe.
                            self.master_password = Some(self.vault_ui.password_input.clone());
                            // Keep the OS-keystore copy current so biometric
                            // unlock reflects the live password (self-heals
                            // after a rotation). No-op unless opted in;
                            // enroll never prompts.
                            self.biometric_reenroll(&self.vault_ui.password_input);
                            self.vault_ui.password_input.clear();
                            self.vault_ui.password_visible = false;
                            // Next lock screen leads with biometrics again
                            // (a one-time fallback choice shouldn't stick).
                            self.vault_ui.password_fallback = false;
                            self.load_data_from_vault();
                            // What the recordings spooled under a soft
                            // lock goes into the vault now, ahead of the
                            // live panes' buffers. Explicit, because the
                            // flush tick only remounts while some pane
                            // is still recording, and a session closed
                            // under the lock left a spool with no pane.
                            self.flush_session_logs();
                            // Re-arm the ssh-agent's dedicated handle if a
                            // runtime survived a soft lock.
                            self.agent_on_unlock();
                            // Bring the sync engine up now that the
                            // vault is open, if the user left it on. Only
                            // the P2P transport has a background engine;
                            // the snapshot transports reconcile on the
                            // cadence subscription.
                            let sync_task = if self.sync.enabled && self.sync_uses_p2p() {
                                self.start_sync_engine()
                            } else {
                                Task::none()
                            };
                            // Auto-start port forward rules now that the
                            // vault (and its credentials) is open.
                            let mut unlock_tasks = vec![sync_task];
                            unlock_tasks.extend(self.auto_start_port_forwards());
                            // The git card's availability probe spawns a
                            // subprocess, so it runs as a task (never in
                            // `view()`). Same probe the boot path and
                            // `TransportChanged` fire.
                            if self.sync.transport == "git" {
                                unlock_tasks
                                    .push(crate::dispatch_git_sync::git_availability_task());
                            }
                            // Plugin migrate-install + auto-update: for a
                            // password vault these are deferred from boot
                            // to here, now that the plugin rows are loaded
                            // (boot saw a locked vault with no rows).
                            unlock_tasks.extend(self.spawn_plugin_unlock_tasks());
                            // The terminal font pack heal, now that the
                            // picked family and weight are known.
                            unlock_tasks.extend(self.unlock_fetch_tasks());
                            // One-time performance-mode auto-enable notice.
                            unlock_tasks.push(self.take_perf_mode_toast_task());
                            // Bring the ssh-agent up if the user left it on.
                            unlock_tasks.push(self.agent_boot_task());
                            // The lock sweep dropped the History content-search
                            // results (decrypted excerpts must not sit behind the
                            // lock screen) but the chip and the typed query survive,
                            // consistent with the soft-lock promise that state comes
                            // back. Re-arm the debounced search here so an active
                            // chip reflects live results again instead of rendering
                            // active over nothing until the next keystroke.
                            if self.history_search_content {
                                unlock_tasks.push(self.history_content_debounce());
                            }
                            // After a manual unlock, fire any deferred
                            // `--connect <uuid>` from the launch CLI args.
                            if let Some(connect_id) = self.pending_auto_connect.take()
                                && let Some(idx) = self
                                    .connections
                                    .iter()
                                    .position(|c| c.id == connect_id)
                            {
                                unlock_tasks.push(Task::done(Message::Ssh(SshMessage::ConnectSsh(idx))));
                            } else if let Some(link) = self.pending_deep_link.take() {
                                // A deep link clicked at the lock screen
                                // routes now, and its own navigation
                                // replaces the default landing focus.
                                let route = self.handle_deep_link(link);
                                unlock_tasks.push(route);
                            } else if let Some(target) = self.pending_connect_target.take() {
                                // `oryxis user@host` launched into a locked
                                // vault: dial now that credentials and
                                // known_hosts are readable.
                                let route = self.handle_connect_target(&target);
                                unlock_tasks.push(route);
                            } else {
                                // Land on Home with the host search focused
                                // so the user can type / keyboard-navigate
                                // immediately (matches ChangeView behavior).
                                unlock_tasks.push(crate::widgets::focus_input(
                                    iced::widget::Id::new("search-dashboard"),
                                ));
                            }
                            return Task::batch(unlock_tasks);
                        }
                        Err(VaultError::InvalidPassword) => {
                            self.vault_ui.error = Some("Invalid password".into());
                        }
                        Err(e) => {
                            self.vault_ui.error = Some(e.to_string());
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

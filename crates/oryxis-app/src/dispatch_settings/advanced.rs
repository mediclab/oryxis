//! Settings dispatch helpers: advanced. Download mirror, debug
//! logging, renderer backend and relaunch. Split out of
//! dispatch_settings/mod.rs.

use super::*;

/// Dispatch `Message::ToastClear` after the standard 1.8s toast dwell,
/// same cadence as the copy-to-clipboard confirmation.
fn toast_clear_task() -> Task<Message> {
    Task::perform(
        async {
            tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
        },
        |_| Message::ToastClear,
    )
}

impl Oryxis {
    /// Task that asks iced for the graphics backend the compositor
    /// actually selected, but only when a settings section that displays
    /// it (Interface, plus Advanced for the environment report) is
    /// showing and it hasn't loaded yet. By then the compositor exists,
    /// so the oneshot resolves instead of being dropped. Returns
    /// [`Task::none`] otherwise. Fired both when switching into the
    /// section and when opening Settings on it.
    pub(crate) fn renderer_info_task(&self) -> Task<Message> {
        if matches!(
            self.settings_section,
            crate::state::SettingsSection::Interface | crate::state::SettingsSection::Advanced
        ) && self.renderer_active.is_none()
        {
            iced::system::graphics_information()
                .map(|info| Message::Settings(SettingsMessage::RendererInfoLoaded(info.backend, info.adapter)))
        } else {
            Task::none()
        }
    }
}

impl Oryxis {
    /// Advanced arms: download mirror, debug logging, the renderer
    /// backend picker and app relaunch.
    pub(super) fn handle_settings_advanced(
        &mut self,
        message: SettingsMessage,
    ) -> Result<Task<Message>, SettingsMessage> {
        match message {
            SettingsMessage::RendererInfoLoaded(backend, adapter) => {
                self.renderer_active = Some((backend, adapter));
            }
            SettingsMessage::SettingRendererBackendChanged(mode) => {
                // No-op if the pick didn't change (re-selecting the same
                // option shouldn't nag about a restart).
                if mode == self.prefs.renderer_backend {
                    return Ok(Task::none());
                }
                self.prefs.renderer_backend = mode.clone();
                self.persist_setting("renderer_backend", &mode);
                // The backend is read once at process start, so the change
                // only takes effect on the next launch. Offer to restart
                // now (applies immediately) or later (applies on next open).
                self.error_dialog = Some(crate::state::ErrorDialog {
                    title: crate::i18n::t("renderer_restart_title").to_string(),
                    body: crate::i18n::t("renderer_restart_body").to_string(),
                    link: None,
                    action: Some(crate::state::ErrorDialogAction {
                        label: crate::i18n::t("renderer_restart_now").to_string(),
                        message: Box::new(Message::Settings(SettingsMessage::RelaunchApp)),
                        // A restart closes every live session, so with
                        // any open it is the destructive answer: styled
                        // as one and NOT the default row, so a stray
                        // Enter dismisses instead. With nothing live it
                        // costs nothing and Enter may take it.
                        danger: self.live_session_tab_count() > 0,
                    }),
                });
            }
            SettingsMessage::RelaunchApp => {
                // Spawns a fresh process and exits this one; never returns
                // on success. If the spawn fails it falls through and the
                // app keeps running.
                self.relaunch_self();
            }
            SettingsMessage::SettingToggleDebugLogging => {
                if crate::logging::is_forced() {
                    // --debug-log pinned the sink on. Say so instead of
                    // flipping a switch that `logging::disable` ignores,
                    // which would leave the row lying about the state.
                    self.set_toast(crate::i18n::t("debug_logging_forced").to_string());
                    return Ok(toast_clear_task());
                }
                if self.prefs.debug_logging {
                    // Emitted before the sink closes so the file records
                    // its own switch-off.
                    tracing::info!("debug logging disabled from Settings");
                    crate::logging::disable();
                    self.prefs.debug_logging = false;
                } else {
                    match crate::logging::enable() {
                        Ok(path) => {
                            self.prefs.debug_logging = true;
                            tracing::info!("debug logging enabled -> {}", path.display());
                        }
                        Err(e) => {
                            // Leave the toggle off and surface the cause;
                            // a silently dead sink would defeat the whole
                            // point of the feature.
                            tracing::warn!("failed to enable debug logging: {e}");
                            self.set_toast(format!("{}: {e}", crate::i18n::t("debug_logging")));
                            return Ok(toast_clear_task());
                        }
                    }
                }
                self.persist_setting(
                    "debug_logging",
                    if self.prefs.debug_logging { "true" } else { "false" },
                );
            }
            SettingsMessage::DownloadMirrorPicked(which) => {
                use crate::net_mirror::MirrorChoice;
                self.download_mirror.test_result = None;
                match which.as_str() {
                    "custom" => {
                        // Open the URL field; nothing persists until a
                        // valid https URL is committed.
                        self.download_mirror.custom_pending = true;
                        if let MirrorChoice::Custom(url) = &self.download_mirror.choice {
                            self.download_mirror.url_input = url.clone();
                        }
                    }
                    token => {
                        let choice = match token {
                            "github" => MirrorChoice::GitHubDirect,
                            "project" => MirrorChoice::ProjectMirror,
                            _ => MirrorChoice::Auto,
                        };
                        self.download_mirror.custom_pending = false;
                        self.download_mirror.url_error = false;
                        self.download_mirror.choice = choice.clone();
                        crate::net_mirror::set_choice(choice.clone());
                        self.persist_setting("download_mirror", &choice.as_setting());
                    }
                }
            }
            SettingsMessage::DownloadMirrorUrlEdited(url) => {
                self.download_mirror.url_input = url;
                self.download_mirror.url_error = false;
                self.download_mirror.test_result = None;
            }
            SettingsMessage::DownloadMirrorUrlCommitted => {
                use crate::net_mirror::MirrorChoice;
                match crate::net_mirror::validate_base(&self.download_mirror.url_input) {
                    Ok(base) => {
                        let choice = MirrorChoice::Custom(base.clone());
                        self.download_mirror.url_input = base;
                        self.download_mirror.url_error = false;
                        self.download_mirror.custom_pending = false;
                        self.download_mirror.choice = choice.clone();
                        crate::net_mirror::set_choice(choice.clone());
                        self.persist_setting("download_mirror", &choice.as_setting());
                    }
                    Err(()) => {
                        self.download_mirror.url_error = true;
                    }
                }
            }
            SettingsMessage::DownloadMirrorTest => {
                use crate::net_mirror::MirrorChoice;
                // The block is hidden while offline mode is on; a stale
                // keyboard row still lands here, and answers in place.
                if self.prefs.offline_mode {
                    self.download_mirror.test_result =
                        Some(Err(crate::i18n::t("offline_mode").to_string()));
                    return Ok(Task::none());
                }
                // Custom is tested against the field's LIVE contents,
                // which may not be committed yet (testing before
                // saving is the point of the button); every other mode
                // owns a fixed endpoint, so the choice resolves it.
                let editing_custom = self.download_mirror.custom_pending
                    || matches!(self.download_mirror.choice, MirrorChoice::Custom(_));
                let target = if editing_custom {
                    match crate::net_mirror::validate_base(&self.download_mirror.url_input) {
                        Ok(base) => {
                            crate::net_mirror::probe_target(&MirrorChoice::Custom(base))
                        }
                        Err(()) => {
                            self.download_mirror.url_error = true;
                            None
                        }
                    }
                } else {
                    crate::net_mirror::probe_target(&self.download_mirror.choice)
                };
                if let Some(url) = target {
                    self.download_mirror.testing = true;
                    self.download_mirror.test_result = None;
                    return Ok(iced::Task::perform(
                        crate::net_mirror::probe(url),
                        |v| Message::Settings(SettingsMessage::DownloadMirrorTestResult(v)),
                    ));
                }
            }
            SettingsMessage::DownloadMirrorTestResult(result) => {
                self.download_mirror.testing = false;
                self.download_mirror.test_result = Some(result);
            }
            SettingsMessage::RevealDebugLog => {
                if let Some(path) = crate::logging::log_path() {
                    // Fall back to the data folder when nothing was
                    // written yet so the button never silently no-ops.
                    let result = if path.exists() {
                        crate::util::reveal_in_file_manager(&path, false)
                    } else if let Some(dir) = path.parent() {
                        crate::util::reveal_in_file_manager(dir, true)
                    } else {
                        Ok(())
                    };
                    if let Err(e) = result {
                        tracing::warn!("failed to reveal debug log: {e}");
                    }
                }
            }
            SettingsMessage::ClearDebugLog => {
                match crate::logging::clear() {
                    Ok(true) => {
                        self.set_toast(crate::i18n::t("debug_log_cleared").to_string());
                    }
                    Ok(false) => {
                        self.set_toast(crate::i18n::t("debug_log_missing").to_string());
                    }
                    Err(e) => {
                        tracing::warn!("failed to clear debug log: {e}");
                        self.set_toast(format!("{}: {e}", crate::i18n::t("debug_log_clear")));
                    }
                }
                return Ok(toast_clear_task());
            }
            m => return Err(m),
        }
        Ok(Task::none())
    }
}

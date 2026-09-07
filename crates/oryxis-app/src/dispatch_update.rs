//! `Oryxis::handle_update`: match arms for the auto-update machinery
//! (check settings + channel, manual/boot checks, download + install),
//! split out of dispatch_ssh.rs. Returns `Err(message)` for anything
//! it doesn't claim so the try_handler! chain falls through.

// Domain handlers return `Err(Message)` to pass an unclaimed message
// back up the chain. The Message enum is large (~200 bytes) but
// boxing it would force every handler-call site to allocate; the
// pattern is intentional, allow the lint.
#![allow(clippy::result_large_err)]

use iced::Task;

use crate::app::{UpdateMessage, Message, Oryxis};
use crate::util::open_in_browser;

impl Oryxis {
    pub(crate) fn handle_update(
        &mut self,
        message: UpdateMessage,
    ) -> Task<Message> {
        // Inside an MSIX package the Store services the app: WindowsApps
        // is read-only, so running our installer would only produce a
        // second, unpackaged copy. Settings > About hides the whole
        // update panel there, but the boot check fires without any UI, so
        // the check / download / install arms are refused here too. The
        // settings-mutation arms stay reachable (harmless, and they keep
        // the persisted preferences intact for a later unpackaged build).
        if crate::packaged::is_packaged()
            && matches!(
                message,
                UpdateMessage::CheckForUpdate
                    | UpdateMessage::CheckForUpdateManual
                    | UpdateMessage::UpdateCheckResult(_)
                    | UpdateMessage::UpdateStartDownload
                    | UpdateMessage::UpdateDownloadProgress(_)
                    | UpdateMessage::UpdateDownloadComplete(_, _)
                    | UpdateMessage::UpdateInstallNow
            )
        {
            return Task::none();
        }
        match message {
            UpdateMessage::SettingToggleAutoCheckUpdates => {
                self.prefs.auto_check_updates = !self.prefs.auto_check_updates;
                self.persist_setting(
                    "auto_check_updates",
                    if self.prefs.auto_check_updates { "true" } else { "false" },
                );
            }
            UpdateMessage::SettingUpdateChannelChanged(channel) => {
                self.prefs.update_channel = channel;
                self.persist_setting("update_channel", channel.as_setting());
                // A channel switch invalidates any "skip this version" so
                // the user is offered the other stream's build right away.
                if let Some(vault) = &self.vault {
                    let _ = vault.set_setting("skipped_update_version", "");
                }
                // Switching channel is an explicit intent to follow that
                // stream, so re-check immediately (surfacing the same
                // "Checking…" status + toast as a manual check) instead of
                // waiting for the next boot check.
                self.update_error = None;
                self.update_check_status = Some(crate::update::UpdateStatus::Checking);
                self.set_toast(crate::i18n::t("update_check_checking").to_string());
                return Task::perform(
                    crate::update::check_latest_release(channel),
                    |res| match res {
                        Ok(info) => Message::Update(UpdateMessage::UpdateCheckResult(info)),
                        Err(e) => Message::Update(UpdateMessage::UpdateCheckFailed(e.to_string())),
                    },
                );
            }
            UpdateMessage::CheckForUpdate => {
                if !self.prefs.auto_check_updates {
                    return Task::none();
                }
                // Also respect a persisted "skip this version" so we never
                // nag about the same tag twice.
                let skipped = self
                    .vault
                    .as_ref()
                    .and_then(|v| v.get_setting("skipped_update_version").ok().flatten());
                return Task::perform(
                    crate::update::check_latest_release(self.prefs.update_channel),
                    move |res| {
                        match res {
                            Ok(Some(info)) if Some(&info.version) != skipped.as_ref() => {
                                Message::Update(UpdateMessage::UpdateCheckResult(Some(info)))
                            }
                            // Boot check is best-effort: log the failure
                            // but never surface it in the UI.
                            Err(e) => {
                                tracing::warn!("update check failed: {e}");
                                Message::Update(UpdateMessage::UpdateCheckResult(None))
                            }
                            _ => Message::Update(UpdateMessage::UpdateCheckResult(None)),
                        }
                    },
                );
            }
            UpdateMessage::CheckForUpdateManual => {
                // Manual trigger from the settings button OR the burger
                // menu. Navigate to Settings > About so the result
                // (up-to-date / error + retry) is on screen regardless
                // of where the check was fired from (issue #38: the
                // burger-menu path previously looked like a no-op).
                self.panels.burger_menu = false;
                self.editing_hotkey = None;
                self.active_view = crate::state::View::Settings;
                self.settings_section = crate::state::SettingsSection::About;
                self.active_tab = None;
                // Sets the view directly rather than going through
                // ChangeView, so it has to mint the strip entry itself or
                // Settings would show with no chip (issue #120).
                self.ensure_panel_tab(crate::state::PanelKind::Settings);
                self.update_error = None;
                self.update_check_status = Some(crate::update::UpdateStatus::Checking);
                self.set_toast(crate::i18n::t("update_check_checking").to_string());
                if let Some(vault) = &self.vault {
                    let _ = vault.set_setting("skipped_update_version", "");
                }
                return Task::perform(
                    crate::update::check_latest_release(self.prefs.update_channel),
                    |res| match res {
                        Ok(info) => Message::Update(UpdateMessage::UpdateCheckResult(info)),
                        Err(e) => Message::Update(UpdateMessage::UpdateCheckFailed(e.to_string())),
                    },
                );
            }
            UpdateMessage::UpdateCheckResult(info) => {
                match info {
                    Some(i) => {
                        // Surface the new version as a toast too so a
                        // burger-menu-triggered check confirms the
                        // result even before the update modal renders.
                        self.set_toast(format!(
                            "{} {}",
                            crate::i18n::t("update_check_available"),
                            i.version,
                        ));
                        self.pending_update = Some(i);
                        self.update_check_status = None;
                    }
                    None => {
                        // Only surface the "up to date" message if a manual
                        // check is in flight (status was set to Checking).
                        // A silent boot check that finds nothing should not
                        // change the settings UI.
                        if self.update_check_status.is_some() {
                            self.update_check_status =
                                Some(crate::update::UpdateStatus::UpToDate);
                            self.set_toast(format!(
                                "{} ({})",
                                crate::i18n::t("update_check_up_to_date"),
                                env!("CARGO_PKG_VERSION"),
                            ));
                        }
                    }
                }
                // Auto-dismiss the toast after the standard 1.8 s
                // window matches the existing "copied to clipboard"
                // toast cadence so users get consistent feedback timing.
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
                    },
                    |_| Message::ToastClear,
                );
            }
            UpdateMessage::UpdateCheckFailed(cause) => {
                // Same gating as the up-to-date arm: only a manual check
                // (status in flight) reports; boot checks already logged.
                if self.update_check_status.is_some() {
                    self.update_check_status =
                        Some(crate::update::UpdateStatus::Failed(cause.clone()));
                    self.set_toast(format!(
                        "{}: {}",
                        crate::i18n::t("update_check_failed"),
                        cause,
                    ));
                }
                return Task::perform(
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;
                    },
                    |_| Message::ToastClear,
                );
            }
            UpdateMessage::UpdateSkipVersion => {
                if let Some(info) = self.pending_update.take()
                    && let Some(vault) = &self.vault {
                    let _ = vault.set_setting("skipped_update_version", &info.version);
                }
                // A skipped version's download goes with it.
                self.update_ready = None;
            }
            UpdateMessage::UpdateLater => {
                self.pending_update = None;
            }
            UpdateMessage::UpdateOpenRelease => {
                if let Some(info) = &self.pending_update {
                    let _ = open_in_browser(&info.html_url);
                }
            }
            UpdateMessage::UpdateStartDownload => {
                let Some(info) = self.pending_update.clone() else {
                    return Task::none();
                };
                // Already downloaded and waiting (the restart was declined
                // earlier): install it rather than fetch it again.
                if self
                    .update_ready
                    .as_ref()
                    .is_some_and(|r| r.info.version == info.version)
                {
                    return self.offer_update_install();
                }
                let Some(url) = info.installer_url.clone() else {
                    self.update_error = Some("No installer asset for this platform".into());
                    return Task::none();
                };
                let name = info
                    .installer_name
                    .clone()
                    .unwrap_or_else(|| format!("oryxis-update-{}", info.version));
                self.update_downloading = true;
                self.update_progress = 0.0;
                self.update_error = None;
                // Stream so the modal's progress bar moves with the
                // download instead of jumping 0 to done. The sync
                // progress closure forwards into the async sink via an
                // unbounded channel.
                // The offer rides the stream, so the completion names the
                // version and artifact kind this file IS, whatever a check
                // made of `pending_update` in the meantime.
                let offer = info.clone();
                let stream = iced::stream::channel::<Message>(
                    100,
                    move |mut sender: iced::futures::channel::mpsc::Sender<Message>| async move {
                        use iced::futures::SinkExt as _;
                        let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<f32>();
                        let mut dl = tokio::spawn(async move {
                            crate::update::download_installer(&url, &name, move |p| {
                                let _ = ptx.send(p);
                            })
                            .await
                        });
                        loop {
                            tokio::select! {
                                Some(p) = prx.recv() => {
                                    let _ = sender
                                        .send(Message::Update(UpdateMessage::UpdateDownloadProgress(p)))
                                        .await;
                                }
                                res = &mut dl => {
                                    let result =
                                        res.unwrap_or_else(|e| Err(e.to_string()));
                                    let _ = sender
                                        .send(Message::Update(UpdateMessage::UpdateDownloadComplete(
                                            Box::new(offer.clone()),
                                            result,
                                        )))
                                        .await;
                                    break;
                                }
                            }
                        }
                    },
                );
                return Task::stream(stream);
            }
            UpdateMessage::UpdateDownloadProgress(p) => {
                self.update_progress = p;
            }
            UpdateMessage::UpdateInstallNow => {
                // From the modal's ready state this IS the answer to the
                // ask. From Settings > About the modal is down, so the
                // offer comes back up first and the gate decides as it
                // did the first time: live sessions put the ask on
                // screen, none means the restart goes ahead.
                if self.pending_update.is_none()
                    && let Some(ready) = &self.update_ready
                {
                    self.pending_update = Some(ready.info.clone());
                    return self.offer_update_install();
                }
                return self.install_ready_update();
            }
            UpdateMessage::UpdateDownloadComplete(offer, result) => {
                self.update_downloading = false;
                match result {
                    Ok(path) => {
                        // The download is only ever applied through the
                        // ready state: the artifact is kept, and whether
                        // it is installed now or after an ask is the
                        // close-window guard's call (`offer_update_install`).
                        // The offer is the one the download was started
                        // for, carried by the message, so a check that
                        // replaced `pending_update` meanwhile (a channel
                        // switch under the modal) cannot pair its own
                        // artifact kind with this file.
                        self.update_ready =
                            Some(crate::update::ReadyUpdate { info: *offer, path });
                        return self.offer_update_install();
                    }
                    Err(e) => self.update_error = Some(e),
                }
            }
        }
        Task::none()
    }

    /// Whether the update offer is on screen, the one predicate its
    /// render site and its `Modal` arm share.
    ///
    /// The error dialog renders inside `view_main` (below the root
    /// overlay), so the offer yields while one is up: at boot a failed
    /// self-update raises the dialog and the update check re-offers the
    /// same build moments later, and without the gate the offer would
    /// cover the failure report it is the consequence of. Dismissing the
    /// dialog reveals the pending offer.
    ///
    /// A DOWNLOAD IN FLIGHT never yields: an unrelated async failure (a
    /// cloud refresh, a dynamic group resolve) can raise a dialog from
    /// any domain at any moment, and hiding the progress surface would
    /// not stop the download, which ends by asking to restart. The app
    /// must not vanish out from under a user reading something else.
    pub(crate) fn update_modal_shown(&self) -> bool {
        self.pending_update.is_some() && (self.update_downloading || self.error_dialog.is_none())
    }

    /// The downloaded update is ready: install it now, or wait for the
    /// user when installing would close live sessions.
    ///
    /// Installing means exiting, so it takes the close-window guard
    /// (`confirm_close_session_tab`, the same opt-in every other close
    /// door honours). With live sessions open the modal shows its ready
    /// state and asks; otherwise the restart the user asked for a
    /// download ago happens on its own.
    fn offer_update_install(&mut self) -> Task<Message> {
        if self.prefs.confirm_close_session_tab && self.live_session_tab_count() > 0 {
            return Task::none();
        }
        self.install_ready_update()
    }

    /// Apply the waiting artifact (`update_ready`) and leave.
    fn install_ready_update(&mut self) -> Task<Message> {
        let Some(ready) = self.update_ready.clone() else {
            return Task::none();
        };
        self.apply_update_artifact(ready.path, ready.info.artifact)
    }

    /// Hand the downloaded artifact to the OS and exit so the old
    /// binary is released.
    ///
    /// Nightly ships a bare binary swapped in place; a portable stable
    /// extracts its exe from the zip and takes the same swap; an
    /// AppImage replaces the image file; an installed stable hands the
    /// downloaded installer to the OS.
    ///
    /// This is an exit door of its own, NOT the close path:
    /// `window::close` is the programmatic action, which removes the
    /// window directly instead of raising the `CloseRequested` the
    /// close subscription listens for, so nothing here passes through
    /// `handle_window_close`. It takes the same teardown by calling it,
    /// which also puts the flushes ahead of an installer that is
    /// already up and waiting for this process to go.
    fn apply_update_artifact(
        &mut self,
        path: std::path::PathBuf,
        artifact: crate::update::UpdateArtifact,
    ) -> Task<Message> {
        use crate::update::UpdateArtifact;
        let apply = match artifact {
            UpdateArtifact::Binary => crate::update::apply_binary_update(&path),
            UpdateArtifact::PortableArchive => crate::update::extract_portable_exe(&path)
                .and_then(|exe| crate::update::apply_binary_update(&exe)),
            UpdateArtifact::AppImage => crate::update::apply_appimage_update(&path),
            UpdateArtifact::Installer => crate::update::launch_installer(&path),
        };
        if let Err(e) = apply {
            self.update_error = Some(e);
            return Task::none();
        }
        self.pending_update = None;
        self.update_ready = None;
        self.persist_before_exit();
        self.drain_plugins_before_exit().then(|_| {
            iced::window::latest().then(|id_opt| match id_opt {
                Some(id) => iced::window::close(id),
                None => Task::none(),
            })
        })
    }
}

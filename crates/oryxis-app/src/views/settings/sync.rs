//! Settings -> Sync section view. Split out of views/settings/mod.rs.

use super::*;
use iced::widget::column;

impl Oryxis {
    /// Placeholder for the shared sync passphrase field. First-time
    /// setup asks what the field is ("shared secret for this sync
    /// group"); edit mode (the user just clicked the masked "saved"
    /// box, so they know a passphrase exists) asks for the replacement
    /// ("enter new passphrase"), the standard change-password wording.
    fn sync_passphrase_placeholder(&self) -> &'static str {
        if self.sync.passphrase_known {
            crate::i18n::t("sync_passphrase_new_placeholder")
        } else {
            crate::i18n::t("sftp_sync_passphrase_placeholder")
        }
    }

    /// The live passphrase-match hint, shown only while the user is
    /// editing the shared group passphrase and a stored one exists:
    /// green when the typed value matches (re-entering the saved key
    /// keeps the group working), warning when it differs. The
    /// comparison itself lives in `set_sync_passphrase`; this only
    /// renders its outcome.
    fn sync_passphrase_hint(&self) -> Option<Element<'_, Message>> {
        match self.sync.passphrase_matches {
            Some(true) => Some(
                text(crate::i18n::t("sync_passphrase_match_hint"))
                    .size(11)
                    .color(OryxisColors::t().success)
                    .into(),
            ),
            Some(false) => Some(
                text(crate::i18n::t("sync_passphrase_mismatch_hint"))
                    .size(11)
                    .color(OryxisColors::t().warning)
                    .into(),
            ),
            None => None,
        }
    }

    /// Recovery hint under a round that failed with a passphrase
    /// mismatch ("Decryption failed (wrong key?)"): the stored key no
    /// longer opens the remote snapshot, and the only way forward is
    /// to discard that snapshot and rebuild it with the passphrase
    /// typed now. Matched on the error text because the failure
    /// arrives as an engine `String`; "Decryption failed" is stable
    /// (pinned by the vault tests).
    fn sync_passphrase_recovery_hint(
        &self,
        status: &Result<String, String>,
    ) -> Option<Element<'_, Message>> {
        let Err(e) = status else {
            return None;
        };
        if !e.contains("Decryption failed") {
            return None;
        }
        Some(
            text(crate::i18n::t("sync_passphrase_forgot_hint"))
                .size(11)
                .color(OryxisColors::t().text_muted)
                .into(),
        )
    }

    /// The shared group-passphrase field block for a snapshot-transport
    /// card. With a stored passphrase the field is READ-ONLY: a plain
    /// masked box with the same geometry as the real input (radius, 1px
    /// border, 10px padding) so the row keeps its shape when it flips to
    /// edit mode, because free typing is what let a mistyped re-entry
    /// swap the group key under the existing snapshot ("Decryption
    /// failed (wrong key?)" on the next round). The box is still a
    /// button: hover border + pointer cursor carry the clickability, and
    /// clicking opens the input, already focused, with the live match
    /// hint; clearing the field abandons the edit (there is no separate
    /// cancel button). The input is also the plain first-time form when
    /// no passphrase is stored yet. The keynav slot covers the whole
    /// read-only row (activate = change); in edit mode the field is the
    /// only stop.
    fn sync_passphrase_block(
        &self,
        id: &'static str,
        width: Length,
    ) -> Element<'_, Message> {
        if self.sync.passphrase_known && !self.sync.passphrase_editing {
            let change = Message::Sync(SyncMessage::PassphraseChangeRequested(id));
            // The display IS the button: no pencil, no tooltip, so it
            // reads exactly like the other fields on the card: a masked
            // value that happens to be clickable. Hover/press are carried
            // by the border and a whisper of background, per the button
            // feedback convention.
            let display = button(
                container(
                    text("\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}")
                        .color(OryxisColors::t().text_muted),
                )
                .width(Length::Fill)
                .padding(10)
                .align_x(dir_align_x()),
            )
            .on_press(change.clone())
            .width(width)
            .padding(0)
            .style(|_t, status| {
                let c = OryxisColors::t();
                let border = match status {
                    BtnStatus::Hovered => c.accent_hover,
                    BtnStatus::Pressed => c.accent,
                    _ => c.border,
                };
                let bg = match status {
                    BtnStatus::Hovered => Color { a: 0.06, ..c.bg_surface },
                    BtnStatus::Pressed => Color { a: 0.10, ..c.bg_surface },
                    _ => c.bg_surface,
                };
                button::Style {
                    background: Some(Background::Color(bg)),
                    text_color: c.text_muted,
                    border: Border {
                        radius: Radius::from(crate::widgets::INPUT_RADIUS),
                        width: 1.0,
                        color: border,
                    },
                    ..Default::default()
                }
            });
            self.settings_nav_slot_labeled(
                t("sftp_sync_passphrase"),
                crate::keynav::RowAction::activate(change),
                8.0,
                display.into(),
            )
        } else {
            // Editable: the input with the live match hint below it.
            // Clearing the field keeps the edit open (the cursor is
            // still in it); a click outside the field abandons it, so
            // there is no separate cancel button. The same form serves
            // first-time setup. The bounds reporter feeds the blur
            // probe (`PassphraseBlurCheck`) with the field's position.
            let field = self.settings_nav_slot_labeled(
                t("sftp_sync_passphrase"),
                crate::keynav::RowAction::input(iced::widget::Id::new(id)),
                crate::widgets::INPUT_RADIUS,
                crate::widgets::bounds_reporter(
                    text_input(
                        self.sync_passphrase_placeholder(),
                        self.sync.passphrase_input.as_str(),
                    )
                    .id(iced::widget::Id::new(id))
                    .on_input(|v| Message::Sync(SyncMessage::PassphraseChanged(v.into())))
                    .secure(true)
                    .padding(10)
                    .width(width)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x()),
                    self.sync.passphrase_field_bounds.clone(),
                ),
            );
            let mut col = column![field];
            if let Some(hint) = self.sync_passphrase_hint() {
                col = col.push(Space::new().height(6)).push(hint);
            }
            col.into()
        }
    }

    /// Engine-state card at the top of the section. Keyboard slots
    /// record during construction, so `view_settings_sync` must call
    /// the per-card builders in on-screen order.
    fn sync_engine_state_card(&self, is_p2p: bool) -> Element<'_, Message> {
        // Enable/disable lives on the Plugins screen now.
        // Live engine state indicator, sits right under the
        // enable toggle so the user sees whether the QUIC /
        // mDNS background tasks are actually up. No snapshot
        // transport runs a background engine, so reporting
        // "Engine stopped" there would read as broken; show a
        // transport-appropriate label instead.
        let engine_state = if !is_p2p {
            let (label, color) = if self.sync.enabled {
                (
                    crate::i18n::t(if self.sync.transport == "sftp" {
                        "sftp_sync_active_label"
                    } else {
                        "snapshot_sync_active_label"
                    }),
                    OryxisColors::t().success,
                )
            } else {
                (
                    crate::i18n::t("sync_engine_stopped_label"),
                    OryxisColors::t().text_muted,
                )
            };
            text(label).size(11).color(color)
        } else if self.sync.engine_running {
            text(crate::i18n::t("sync_engine_running_label"))
                .size(11)
                .color(OryxisColors::t().success)
        } else {
            text(crate::i18n::t("sync_engine_stopped_label"))
                .size(11)
                .color(OryxisColors::t().text_muted)
        };

        // Master enable panel sits at the top, same shape as
        // the Enable SFTP / Enable AI panels: a single toggle
        // (with the engine state hint right under it). When
        // the master toggle is off, every other Sync panel
        // is hidden below so the surface collapses to just
        // the on/off knob.
        let enable_section: iced::widget::Column<'_, Message> = column![
            engine_state,
        ];

        panel_section(enable_section)
    }

    /// SFTP snapshot-transport config block: host, remote path,
    /// passphrase, and the group/known-host notes. Joins the
    /// Transport card right below the method picker.
    fn sync_snapshot_card(&self) -> iced::widget::Column<'_, Message> {
        // Host field opens the same rich "Select a host" modal as
        // the SFTP file browser (OS badge + label + address +
        // search), not a flat dropdown. The trigger shows the
        // current selection or a placeholder.
        let selected_conn = self
            .sync.sftp.host_id
            .and_then(|id| self.connections.iter().find(|c| c.id == id));
        let host_trigger_inner: Element<'_, Message> = if let Some(c) = selected_conn {
            dir_row(vec![
                host_badge(c, &self.prefs.default_host_icon, 22.0),
                Space::new().width(10).into(),
                text(c.label.clone())
                    .size(13)
                    .color(OryxisColors::t().text_primary)
                    .into(),
                Space::new().width(Length::Fill).into(),
                text("\u{25BE}").size(12).color(OryxisColors::t().text_muted).into(),
            ])
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            dir_row(vec![
                text(crate::i18n::t("select_a_host"))
                    .size(13)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                Space::new().width(Length::Fill).into(),
                text("\u{25BE}").size(12).color(OryxisColors::t().text_muted).into(),
            ])
            .align_y(iced::Alignment::Center)
            .into()
        };
        let host_pick = self.settings_nav_slot_labeled(
            t("sftp_sync_host"),
            crate::keynav::RowAction::activate(Message::Sync(SyncMessage::SftpHostPickerOpen)),
            8.0,
            button(host_trigger_inner)
                .on_press(Message::Sync(SyncMessage::SftpHostPickerOpen))
                .padding(10)
                .width(300)
                .style(|_, status| {
                    let c = OryxisColors::t();
                    let border = match status {
                        BtnStatus::Hovered => c.accent_hover,
                        _ => c.border,
                    };
                    button::Style {
                        background: Some(Background::Color(c.bg_surface)),
                        text_color: c.text_primary,
                        border: Border {
                            radius: Radius::from(8.0),
                            width: 1.0,
                            color: border,
                        },
                        ..Default::default()
                    }
                })
                .into(),
        );
        let path_input = self.settings_nav_slot_labeled(
            t("sftp_sync_path"),
            crate::keynav::RowAction::input(iced::widget::Id::new("set-sync-sftp-path")),
            10.0,
            text_input(
                "/home/user/oryxis-sync/",
                &self.sync.sftp.remote_path,
            )
            .id(iced::widget::Id::new("set-sync-sftp-path"))
            .on_input(|v| Message::Sync(SyncMessage::SftpPathChanged(v)))
            .padding(10)
            .width(300)
            .style(crate::widgets::rounded_input_style)
            .align_x(dir_align_x())
            .into(),
        );
        let mut sftp_section_col = column![
            text(crate::i18n::t("sftp_sync_title"))
                .size(13)
                .color(OryxisColors::t().text_primary),
            Space::new().height(8),
            panel_field(crate::i18n::t("sftp_sync_host"), host_pick),
            Space::new().height(8),
            panel_field(crate::i18n::t("sftp_sync_path"), path_input),
            Space::new().height(8),
            panel_field(
                crate::i18n::t("sftp_sync_passphrase"),
                self.sync_passphrase_block("set-sync-sftp-passphrase", Length::Fixed(300.0)),
            ),
        ];
        // (The round status lives under the Sync Now button in the
        // options panel above, not here, so feedback sits next to
        // the control that triggers it.)
        sftp_section_col = sftp_section_col
            .push(Space::new().height(12))
            .push(
                text(crate::i18n::t("sftp_sync_note_group"))
                    .size(11)
                    .color(OryxisColors::t().text_muted),
            )
            .push(Space::new().height(4))
            .push(
                text(crate::i18n::t("sftp_sync_note_bridge"))
                    .size(11)
                    .color(OryxisColors::t().text_muted),
            )
            .push(Space::new().height(4))
            .push(
                text(crate::i18n::t("sftp_sync_note_hostkey"))
                    .size(11)
                    .color(OryxisColors::t().text_muted),
            );
        sftp_section_col
    }

    /// The transports, in picker order: settings token, label key, and
    /// whether the config block below the picker belongs to it.
    ///
    /// A TABLE and not a chain of `if`s, because the chain is what let
    /// the folder and Git transports ship with the P2P surface still
    /// attached: every place that had to name a transport named it by
    /// exclusion instead. Adding one here is a single row.
    const TRANSPORTS: &'static [(&'static str, &'static str)] = &[
        ("p2p", "sync_transport_p2p"),
        ("sftp", "sync_transport_sftp"),
        ("folder", "sync_transport_folder"),
        ("git", "sync_transport_git"),
        ("webdav", "sync_transport_webdav"),
    ];

    /// Transport card: the transport picker plus, under it, the
    /// selected transport's config block.
    fn sync_transport_card(&self, is_sftp: bool) -> Element<'_, Message> {
        // Labels are translated, so the picker round-trips through them
        // and back to the token. Built once and shared by the pick_list
        // and the Left/Right cycle so the two cannot disagree.
        let labels: Vec<String> = Self::TRANSPORTS
            .iter()
            .map(|(_, key)| crate::i18n::t(key).to_string())
            .collect();
        let selected = Self::TRANSPORTS
            .iter()
            .position(|(token, _)| *token == self.sync.transport)
            // An unknown token (a settings row from a newer build)
            // shows as P2P rather than an empty picker.
            .unwrap_or(0);
        let transport_selected = labels[selected].clone();

        let to_message = {
            let labels = labels.clone();
            move |v: String| {
                let token = labels
                    .iter()
                    .position(|l| *l == v)
                    .map_or("p2p", |idx| Self::TRANSPORTS[idx].0);
                Message::Sync(SyncMessage::TransportChanged(token.to_string()))
            }
        };
        // Left/Right cycle the transports without opening the dropdown.
        let (transport_prev, transport_next) = crate::keynav::slots::cycle_pair(
            &labels,
            &transport_selected,
            to_message.clone(),
        );
        let transport_pick = pick_list(
            Some(transport_selected),
            labels,
            |s: &String| s.clone(),
        )
        .on_select(to_message)
        .on_open(Message::Navigation(NavigationMessage::PickOpenChanged(true)))
        .on_close(Message::Navigation(NavigationMessage::PickOpenChanged(false)))
        .text_size(13)
        .padding(10)
        .style(crate::widgets::rounded_pick_list_style);
        let transport_row = self.settings_nav_slot_labeled(
            t("sync_transport_field"),
            crate::keynav::RowAction::picker(transport_prev, transport_next),
            8.0,
            dir_row(vec![
                text(crate::i18n::t("sync_transport_field"))
                    .size(13)
                    .color(OryxisColors::t().text_secondary)
                    .into(),
                Space::new().width(Length::Fill).into(),
                transport_pick.into(),
            ])
            .align_y(iced::Alignment::Center)
            .into(),
        );
        let mut transport_col = column![transport_row];
        // The selected transport's config joins the Transport card so
        // the picker and its settings read as one theme; built here so
        // the keyboard rows record before Options renders.
        let config: Option<Element<'_, Message>> = match self.sync.transport.as_str() {
            _ if is_sftp => Some(self.sync_snapshot_card().into()),
            "folder" => Some(self.sync_folder_card()),
            "git" => Some(self.sync_git_card()),
            "webdav" => Some(self.sync_webdav_card()),
            _ => None,
        };
        if let Some(config) = config {
            transport_col = transport_col
                .push(Space::new().height(16))
                .push(config);
        }
        panel_section(transport_col)
    }

    /// One labelled, keynav-recorded text input for the WebDAV card.
    ///
    /// A method and not a closure because the row borrows `self` for
    /// the keynav ring AND the field value, and closure lifetime
    /// elision cannot express that the two live as long as the element.
    fn webdav_field<'a>(
        &'a self,
        label: &'static str,
        id: &'static str,
        value: &'a str,
        secure: bool,
        to_msg: fn(String) -> Message,
    ) -> Element<'a, Message> {
        self.settings_nav_slot_labeled(
            t(label),
            crate::keynav::RowAction::input(iced::widget::Id::new(id)),
            crate::widgets::INPUT_RADIUS,
            text_input(t(label), value)
                .id(iced::widget::Id::new(id))
                .on_input(to_msg)
                .secure(secure)
                .padding(10)
                .width(Length::Fill)
                .style(crate::widgets::rounded_input_style)
                .align_x(dir_align_x())
                .into(),
        )
    }

    /// WebDAV-transport config: the collection URL, the account, and
    /// the group passphrase.
    ///
    /// Three fields and no OAuth, which is the reason this transport
    /// exists in the first place. The password field is the account's
    /// credential on that server (an app password on Nextcloud), a
    /// different secret from the passphrase under it, and the labels
    /// say so because conflating the two silently puts a device in its
    /// own sync group.
    fn sync_webdav_card(&self) -> Element<'_, Message> {
        let busy = self.sync.webdav.in_progress;
        let mut col = column![
            text(t("webdav_sync_desc"))
                .size(11)
                .color(OryxisColors::t().text_muted),
            Space::new().height(10),
        ];

        for (label, id, value, secure, to_msg, placeholder) in [
            (
                "webdav_sync_url",
                "sync-webdav-url",
                self.sync.webdav.url.as_str(),
                false,
                (|v| Message::Sync(SyncMessage::WebdavUrlChanged(v))) as fn(String) -> Message,
                Some("https://cloud.example.com/remote.php/dav/files/me/oryxis/"),
            ),
            (
                "webdav_sync_user",
                "sync-webdav-user",
                self.sync.webdav.user.as_str(),
                false,
                |v| Message::Sync(SyncMessage::WebdavUserChanged(v)),
                None,
            ),
            (
                "webdav_sync_password",
                "sync-webdav-pass",
                self.sync.webdav.password.as_str(),
                true,
                |v| Message::Sync(SyncMessage::WebdavPasswordChanged(v.into())),
                None,
            ),
        ] {
            col = col
                .push(
                    text(t(label))
                        .size(12)
                        .color(OryxisColors::t().text_secondary),
                )
                .push(Space::new().height(4))
                .push(self.webdav_field(label, id, value, secure, to_msg));
            if let Some(hint) = placeholder {
                col = col.push(Space::new().height(4)).push(
                    text(hint).size(10).color(OryxisColors::t().text_muted),
                );
            }
            col = col.push(Space::new().height(10));
        }
        // The group passphrase is deliberately NOT a webdav_field: with
        // a stored passphrase it is a read-only masked box until the
        // user clicks it to edit (same shared block as the other three
        // transports).
        col = col
            .push(
                text(t("sftp_sync_passphrase"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
            )
            .push(Space::new().height(4))
            .push(self.sync_passphrase_block("sync-webdav-passphrase", Length::Fill))
            .push(Space::new().height(10));

        col = col.push(Space::new().height(2)).push(
            self.settings_nav_slot_labeled(
                t("sync_now"),
                crate::keynav::RowAction::activate(Message::Sync(SyncMessage::WebdavSyncNow)),
                8.0,
                crate::widgets::styled_button_opt(
                    t("sync_now"),
                    (!busy).then_some(Message::Sync(SyncMessage::WebdavSyncNow)),
                    OryxisColors::t().accent,
                ),
            ),
        );

        // Cleartext warning. This is the first transport where a
        // credential crosses the wire at all: SFTP is encrypted, Git
        // hands authentication to ssh or a credential helper, and the
        // folder transport never leaves the machine. Basic auth over
        // `http://` is the account password in base64, readable by
        // anything on the path, so the card says so where the URL is
        // typed instead of leaving it to the manual. Loopback is
        // exempt because there is no path.
        if webdav_is_cleartext(&self.sync.webdav.url) {
            col = col.push(Space::new().height(2)).push(
                text(t("webdav_sync_cleartext"))
                    .size(11)
                    .color(OryxisColors::t().warning),
            );
        }

        if let Some(status) = &self.sync.webdav.status {
            let (msg, color) = match status {
                Ok(s) => (s.clone(), OryxisColors::t().success),
                Err(e) => (e.clone(), OryxisColors::t().error),
            };
            col = col
                .push(Space::new().height(8))
                .push(text(msg).size(11).color(color));
            if let Some(hint) = self.sync_passphrase_recovery_hint(status) {
                col = col.push(Space::new().height(4)).push(hint);
            }
        }

        col = col.push(Space::new().height(10)).push(
            text(t("webdav_sync_cas_note"))
                .size(10)
                .color(OryxisColors::t().text_muted),
        );
        col.into()
    }

    /// Git-transport config: the remote, the group passphrase, and the
    /// one requirement this transport has that no other does.
    ///
    /// The `git` check used to run here (in `view()`) so a machine
    /// without it said so while the user was still choosing. That froze
    /// the UI and flashed a console window per render on Windows (a
    /// subprocess spawn on the UI thread). It now runs as a task
    /// (`GitAvailabilityChecked`) at boot, on transport switch, on
    /// opening the Sync section and after each round; this card only
    /// reads the cached result.
    fn sync_git_card(&self) -> Element<'_, Message> {
        let busy = self.sync.git.in_progress;
        // `None` = probe in flight: the button waits a moment rather
        // than guessing, and no warning is shown until the probe
        // answers.
        let has_git = self.sync.git.git_available.unwrap_or(false);
        let mut col = column![
            text(t("git_sync_desc"))
                .size(11)
                .color(OryxisColors::t().text_muted),
            Space::new().height(10),
        ];

        if self.sync.git.git_available == Some(false) {
            col = col
                .push(
                    text(t("git_sync_no_git"))
                        .size(11)
                        .color(OryxisColors::t().warning),
                )
                .push(Space::new().height(10));
        }

        col = col
            .push(
                text(t("git_sync_remote"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
            )
            .push(Space::new().height(4))
            .push(self.settings_nav_slot_labeled(
                t("git_sync_remote"),
                crate::keynav::RowAction::input(iced::widget::Id::new("sync-git-remote")),
                crate::widgets::INPUT_RADIUS,
                text_input("git@github.com:me/oryxis-vault.git", &self.sync.git.remote)
                    .id(iced::widget::Id::new("sync-git-remote"))
                    .on_input(|v| Message::Sync(SyncMessage::GitRemoteChanged(v)))
                    .padding(10)
                    .width(Length::Fill)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x())
                    .into(),
            ))
            .push(Space::new().height(10))
            .push(
                text(t("sftp_sync_passphrase"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
            )
            .push(Space::new().height(4))
            .push(self.sync_passphrase_block("sync-git-pass", Length::Fill))
            .push(Space::new().height(12));

        col = col.push(self.settings_nav_slot_labeled(
            t("sync_now"),
            crate::keynav::RowAction::activate(Message::Sync(SyncMessage::GitSyncNow)),
            8.0,
            crate::widgets::styled_button_opt(
                t("sync_now"),
                (!busy && has_git).then_some(Message::Sync(SyncMessage::GitSyncNow)),
                OryxisColors::t().accent,
            ),
        ));

        if let Some(status) = &self.sync.git.status {
            let (msg, color) = match status {
                Ok(s) => (s.clone(), OryxisColors::t().success),
                Err(e) => (e.clone(), OryxisColors::t().error),
            };
            col = col
                .push(Space::new().height(8))
                .push(text(msg).size(11).color(color));
            if let Some(hint) = self.sync_passphrase_recovery_hint(status) {
                col = col.push(Space::new().height(4)).push(hint);
            }
        }

        col = col.push(Space::new().height(10)).push(
            text(t("git_sync_history_note"))
                .size(10)
                .color(OryxisColors::t().text_muted),
        );
        col.into()
    }

    /// Folder-transport config: where the snapshot lives, the group
    /// passphrase, and the one caveat worth stating up front.
    ///
    /// The path is the whole feature. Whatever the OS mounts becomes a
    /// sync destination, so the field is deliberately a plain path with
    /// a picker rather than a provider list: adding OneDrive or Drive
    /// here would suggest Oryxis talks to them, when in fact it never
    /// leaves the filesystem.
    fn sync_folder_card(&self) -> Element<'_, Message> {
        let busy = self.sync.folder.in_progress;
        let mut col = column![
            text(t("folder_sync_desc"))
                .size(11)
                .color(OryxisColors::t().text_muted),
            Space::new().height(10),
        ];

        let path_row = dir_row(vec![
            self.settings_nav_slot_labeled(
                t("folder_sync_path"),
                crate::keynav::RowAction::input(iced::widget::Id::new("sync-folder-path")),
                crate::widgets::INPUT_RADIUS,
                text_input(t("folder_sync_path"), &self.sync.folder.path)
                    .id(iced::widget::Id::new("sync-folder-path"))
                    .on_input(|v| Message::Sync(SyncMessage::FolderPathChanged(v)))
                    .padding(10)
                    .width(Length::Fill)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x())
                    .into(),
            ),
            Space::new().width(8).into(),
            self.settings_nav_slot_labeled(
                t("folder_sync_pick"),
                crate::keynav::RowAction::activate(Message::Sync(
                    SyncMessage::FolderPickDirectory,
                )),
                8.0,
                crate::widgets::styled_button(
                    t("folder_sync_pick"),
                    Message::Sync(SyncMessage::FolderPickDirectory),
                    OryxisColors::t().text_secondary,
                ),
            ),
        ])
        .align_y(iced::Alignment::Center);
        col = col.push(path_row).push(Space::new().height(10));

        // Same group passphrase as the SFTP transport, deliberately:
        // it derives the snapshot key, so two devices that disagree
        // here are two different sync groups pointed at one file.
        col = col
            .push(
                text(t("sftp_sync_passphrase"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary),
            )
            .push(Space::new().height(4))
            .push(self.sync_passphrase_block("sync-folder-pass", Length::Fill))
            .push(Space::new().height(12));

        col = col.push(self.settings_nav_slot_labeled(
            t("sync_now"),
            crate::keynav::RowAction::activate(Message::Sync(SyncMessage::FolderSyncNow)),
            8.0,
            crate::widgets::styled_button_opt(
                t("sync_now"),
                (!busy).then_some(Message::Sync(SyncMessage::FolderSyncNow)),
                OryxisColors::t().accent,
            ),
        ));

        if let Some(status) = &self.sync.folder.status {
            let (msg, color) = match status {
                Ok(s) => (s.clone(), OryxisColors::t().success),
                Err(e) => (e.clone(), OryxisColors::t().error),
            };
            col = col
                .push(Space::new().height(8))
                .push(text(msg).size(11).color(color));
            if let Some(hint) = self.sync_passphrase_recovery_hint(status) {
                col = col.push(Space::new().height(4)).push(hint);
            }
        }

        // Stated in the UI rather than buried in docs: a user pointing
        // two machines at one cloud folder is choosing a trade-off, and
        // they can only choose it if they know it exists.
        col = col.push(Space::new().height(10)).push(
            text(t("folder_sync_shared_warning"))
                .size(10)
                .color(OryxisColors::t().text_muted),
        );
        col.into()
    }

    /// Options card: the Auto/Manual mode picker, the password-sync
    /// toggle, the Sync Now / Cancel action and the status + health
    /// lines under it.
    fn sync_options_card(&self, is_sftp: bool, is_p2p: bool) -> Element<'_, Message> {
        let mode_label = if self.sync.mode == "auto" { t("sync_mode_auto") } else { t("sync_mode_manual") };
        let auto_label = t("sync_mode_auto").to_string();
        let manual_label = t("sync_mode_manual").to_string();
        let mode_options = vec![auto_label.clone(), manual_label.clone()];
        let mode_selected = mode_label.to_string();
        // Left/Right cycle Auto/Manual with the same mapping as
        // the dropdown's on_select below.
        let auto_label_for_cycle = auto_label.clone();
        let (mode_prev, mode_next) = crate::keynav::slots::cycle_pair(
            &mode_options,
            &mode_selected,
            move |v: String| {
                let mode = if v == auto_label_for_cycle || v == "Auto" {
                    "auto"
                } else {
                    "manual"
                };
                Message::Sync(SyncMessage::ModeChanged(mode.to_string()))
            },
        );
        let mode_pick = pick_list(
            Some(mode_selected),
            mode_options,
            |s: &String| s.clone(),
        )
        .on_select(move |v| {
            // Compare against localized labels first; fall back
            // to English so labels persisted in another locale
            // still resolve to a known mode.
            let mode = if v == auto_label || v == "Auto" {
                "auto"
            } else {
                "manual"
            };
            Message::Sync(SyncMessage::ModeChanged(mode.to_string()))
        })
        .on_open(Message::Navigation(NavigationMessage::PickOpenChanged(true)))
        .on_close(Message::Navigation(NavigationMessage::PickOpenChanged(false)))
        .text_size(13)
        .padding(10)
        .style(crate::widgets::rounded_pick_list_style);
        let mode_row = self.settings_nav_slot_labeled(
            t("sync_mode"),
            crate::keynav::RowAction::picker(mode_prev, mode_next),
            8.0,
            dir_row(vec![
                text(crate::i18n::t("sync_mode")).size(13).color(OryxisColors::t().text_secondary).into(),
                Space::new().width(Length::Fill).into(),
                mode_pick.into(),
            ])
            .align_y(iced::Alignment::Center)
            .into(),
        );

        let passwords_toggle = self.nav_toggle_row(
            crate::i18n::t("sync_passwords"),
            self.sync.passwords,
            Message::Sync(SyncMessage::TogglePasswords),
        );

        let mut options_section: iced::widget::Column<'_, Message> = column![
            mode_row,
            Space::new().height(8),
            passwords_toggle,
            Space::new().height(4),
            text(crate::i18n::t("sync_passwords_desc"))
                .size(11)
                .color(OryxisColors::t().text_muted),
        ];

        // Manual mode's action button. SFTP and P2P live here; the
        // folder, Git and WebDAV cards carry their own "Sync now" next
        // to the config it acts on, so a second one here would be a
        // duplicate (and, before the transport gates were fixed, a
        // button that fired the P2P path and did nothing at all).
        if self.sync.mode == "manual" && (is_sftp || is_p2p) {
            if is_sftp {
                // SFTP round: relabel + disable the button while a
                // round is in flight so the click has immediate
                // feedback. There's no engine/Cancel path (the
                // transfer can't be safely aborted mid-write).
                // Recorded only while enabled, so Enter can't
                // re-fire a round already in flight.
                let (label, msg) = if self.sync.sftp.in_progress {
                    (crate::i18n::t("sftp_sync_running"), None)
                } else {
                    (crate::i18n::t("sync_now"), Some(Message::Sync(SyncMessage::Now)))
                };
                let sync_btn =
                    styled_button_opt(label, msg.clone(), OryxisColors::t().accent);
                let sync_btn: Element<'_, Message> = if let Some(m) = msg {
                    self.settings_nav_slot_labeled(
                        t("sync_now"),
                        crate::keynav::RowAction::activate(m),
                        6.0,
                        sync_btn,
                    )
                } else {
                    sync_btn
                };
                options_section =
                    options_section.push(Space::new().height(8)).push(sync_btn);
            } else {
                // P2P: swap Sync Now <-> Cancel while a sync is in
                // flight. Cancel races a oneshot against the sync
                // future in dispatch; the click drops the QUIC
                // connection immediately.
                let action_btn = if self.sync.in_progress {
                    self.settings_nav_slot_labeled(
                        t("sync_now"),
                        crate::keynav::RowAction::activate(Message::Sync(SyncMessage::CancelInProgress)),
                        6.0,
                        styled_button(
                            crate::i18n::t("sync_pairing_cancel"),
                            Message::Sync(SyncMessage::CancelInProgress),
                            OryxisColors::t().button_bg,
                        ),
                    )
                } else {
                    self.settings_nav_slot_labeled(
                        t("sync_now"),
                        crate::keynav::RowAction::activate(Message::Sync(SyncMessage::Now)),
                        6.0,
                        styled_button(
                            crate::i18n::t("sync_now"),
                            Message::Sync(SyncMessage::Now),
                            OryxisColors::t().accent,
                        ),
                    )
                };
                options_section = options_section
                    .push(Space::new().height(8))
                    .push(action_btn);
            }
        }

        // Status line directly under the action button. SFTP shows
        // its own round outcome (success muted / error red); P2P
        // keeps the engine status string. The other snapshot
        // transports report inside their own card.
        if is_sftp {
            if let Some(status) = &self.sync.sftp.status {
                let (txt, color) = match status {
                    Ok(s) => (s.clone(), OryxisColors::t().text_muted),
                    Err(e) => (e.clone(), OryxisColors::t().error),
                };
                options_section = options_section
                    .push(Space::new().height(8))
                    .push(text(txt).size(12).color(color));
                if let Some(hint) = self.sync_passphrase_recovery_hint(status) {
                    options_section = options_section
                        .push(Space::new().height(4))
                        .push(hint);
                }
            }
        } else if is_p2p
            && let Some(status) = &self.sync.status
        {
            options_section = options_section
                .push(Space::new().height(8))
                .push(text(status.as_str()).size(12).color(OryxisColors::t().text_muted));
        }

        // Persistent P2P health under the (transient) status line:
        // LAN discovery count plus the LAST signaling outcome, which
        // survives later events overwriting `status`, so "signaling
        // has been failing" stays visible instead of flashing once.
        if is_p2p {
            options_section = options_section.push(Space::new().height(8)).push(
                text(format!(
                    "{} {}",
                    self.sync.discovered.len(),
                    crate::i18n::t("sync_health_lan"),
                ))
                .size(11)
                .color(OryxisColors::t().text_muted),
            );
            if let Some(sig) = &self.sync.signaling_last {
                let (txt, color) = match sig {
                    Ok(addr) => (
                        format!(
                            "{}: {addr}",
                            crate::i18n::t("sync_status_signaling_registered"),
                        ),
                        OryxisColors::t().text_muted,
                    ),
                    Err(reason) => (
                        format!(
                            "{}: {reason}",
                            crate::i18n::t("sync_status_signaling_failed"),
                        ),
                        OryxisColors::t().error,
                    ),
                };
                options_section = options_section
                    .push(Space::new().height(4))
                    .push(text(txt).size(11).color(color));
            }
        }

        panel_section(options_section)
    }

    /// Devices card (P2P only): the device-name field plus the
    /// pairing block (entry buttons / hosting code / join form per
    /// `sync_pairing.state`) and the paired-devices list.
    fn sync_devices_card(&self) -> Element<'_, Message> {
        // Device info
        let device_name_input = self.settings_nav_slot_labeled(
            t("sync_device_name"),
            crate::keynav::RowAction::input(iced::widget::Id::new(
                "set-sync-device-name",
            )),
            10.0,
            text_input(
                crate::i18n::t("sync_device_name_hint"),
                &self.sync.device_name,
            )
            .id(iced::widget::Id::new("set-sync-device-name"))
            .on_input(|v| Message::Sync(SyncMessage::DeviceNameChanged(v)))
            .padding(10)
            .width(300)
            .style(crate::widgets::rounded_input_style).align_x(dir_align_x())
            .into(),
        );

        let device_col = column![
            text(crate::i18n::t("sync_device_name")).size(12).color(OryxisColors::t().text_muted),
            Space::new().height(4),
            device_name_input,
        ];

        // Pairing. The sub-view depends on `sync_pairing.state`:
        // Idle shows the two entry buttons; Hosting shows the
        // generated code; Joining shows the code + address form.
        let mut pairing_section: iced::widget::Column<'_, Message> = column![
            text(crate::i18n::t("sync_pairing")).size(13).color(OryxisColors::t().text_primary),
            Space::new().height(8),
        ];

        match self.sync.pairing.state {
            crate::state::SyncPairingState::Idle => {
                pairing_section = pairing_section.push(dir_row(vec![
                    self.settings_nav_slot_labeled(
                        t("sync_host_pairing"),
                        crate::keynav::RowAction::activate(Message::Sync(SyncMessage::StartPairing)),
                        6.0,
                        styled_button(
                            crate::i18n::t("sync_host_pairing"),
                            Message::Sync(SyncMessage::StartPairing),
                            OryxisColors::t().accent,
                        ),
                    ),
                    Space::new().width(8).into(),
                    self.settings_nav_slot_labeled(
                        t("sync_join_pairing"),
                        crate::keynav::RowAction::activate(
                            Message::Sync(SyncMessage::JoinPairingRequested),
                        ),
                        6.0,
                        styled_button(
                            crate::i18n::t("sync_join_pairing"),
                            Message::Sync(SyncMessage::JoinPairingRequested),
                            OryxisColors::t().button_bg,
                        ),
                    ),
                ]));
                // Live mDNS-discovered devices on the LAN.
                // One-click "Pair" switches to the join form
                // with the address pre-filled, so the user
                // only has to enter the 6-digit code.
                if !self.sync.discovered.is_empty() {
                    pairing_section = pairing_section
                        .push(Space::new().height(14))
                        .push(text(crate::i18n::t("sync_discovered_devices"))
                            .size(12)
                            .color(OryxisColors::t().text_secondary))
                        .push(Space::new().height(6));
                    for peer in &self.sync.discovered {
                        let label = if peer.device_name.is_empty() {
                            crate::i18n::t("sync_discovered_unnamed").to_string()
                        } else {
                            peer.device_name.clone()
                        };
                        let pair_btn = self.settings_nav_slot(
                            crate::keynav::RowAction::activate(
                                Message::Sync(SyncMessage::PairWithDiscovered(peer.device_id)),
                            ),
                            6.0,
                            styled_button(
                                crate::i18n::t("sync_pair_with_this"),
                                Message::Sync(SyncMessage::PairWithDiscovered(peer.device_id)),
                                OryxisColors::t().button_bg,
                            ),
                        );
                        pairing_section = pairing_section
                            .push(dir_row(vec![
                                text(label)
                                    .size(13)
                                    .color(OryxisColors::t().text_primary)
                                    .into(),
                                Space::new().width(8).into(),
                                text(peer.addr.to_string())
                                    .size(11)
                                    .color(OryxisColors::t().text_muted)
                                    .into(),
                                Space::new().width(Length::Fill).into(),
                                pair_btn,
                            ])
                            .align_y(iced::Alignment::Center))
                            .push(Space::new().height(4));
                    }
                }
            }
            crate::state::SyncPairingState::Hosting => {
                pairing_section = pairing_section
                    .push(text(crate::i18n::t("sync_pairing_show_code"))
                        .size(12)
                        .color(OryxisColors::t().text_secondary))
                    .push(Space::new().height(6));
                if let Some(code) = &self.sync.pairing.code {
                    pairing_section = pairing_section
                        .push(text(code.as_str())
                            .size(30)
                            .color(OryxisColors::t().success));
                }
                // Cross-network pairing block: the link + a
                // Copy button + the QR. The link works only
                // when both ends have a signaling URL set
                // (Settings > Sync > Advanced).
                if let Some(link) = &self.sync.pairing.link {
                    pairing_section = pairing_section
                        .push(Space::new().height(12))
                        .push(text(crate::i18n::t("sync_pairing_link_label"))
                            .size(12)
                            .color(OryxisColors::t().text_secondary))
                        .push(Space::new().height(4))
                        .push(text(link.as_str())
                            .size(11)
                            .color(OryxisColors::t().text_muted))
                        .push(Space::new().height(6))
                        .push(self.settings_nav_slot(
                            crate::keynav::RowAction::activate(
                                Message::CopyToClipboard(link.clone()),
                            ),
                            6.0,
                            styled_button(
                                crate::i18n::t("sync_pairing_copy_link"),
                                Message::CopyToClipboard(link.clone()),
                                OryxisColors::t().button_bg,
                            ),
                        ));
                }
                pairing_section = pairing_section
                    .push(Space::new().height(12))
                    .push(self.settings_nav_slot(
                        crate::keynav::RowAction::activate(
                            Message::Sync(SyncMessage::CancelHostingPairing),
                        ),
                        6.0,
                        styled_button(
                            crate::i18n::t("sync_pairing_cancel"),
                            Message::Sync(SyncMessage::CancelHostingPairing),
                            OryxisColors::t().button_bg,
                        ),
                    ));
            }
            crate::state::SyncPairingState::Joining => {
                let code_input = self.settings_nav_slot(
                    crate::keynav::RowAction::input(iced::widget::Id::new(
                        "set-sync-pair-code",
                    )),
                    10.0,
                    text_input(
                        crate::i18n::t("sync_pairing_code_placeholder"),
                        &self.sync.pairing.join_code_input,
                    )
                    .id(iced::widget::Id::new("set-sync-pair-code"))
                    .on_input(|v| Message::Sync(SyncMessage::JoinCodeChanged(v)))
                    .padding(8)
                    .width(280)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x())
                    .into(),
                );
                let target_input = self.settings_nav_slot(
                    crate::keynav::RowAction::input(iced::widget::Id::new(
                        "set-sync-pair-target",
                    )),
                    10.0,
                    text_input(
                        crate::i18n::t("sync_pairing_target_placeholder"),
                        &self.sync.pairing.join_target_input,
                    )
                    .id(iced::widget::Id::new("set-sync-pair-target"))
                    .on_input(|v| Message::Sync(SyncMessage::JoinTargetChanged(v)))
                    .padding(8)
                    .width(320)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x())
                    .into(),
                );
                let connect_btn = self.settings_nav_slot(
                    crate::keynav::RowAction::activate(Message::Sync(SyncMessage::JoinPairingConnect)),
                    6.0,
                    styled_button(
                        crate::i18n::t("sync_pairing_connect"),
                        Message::Sync(SyncMessage::JoinPairingConnect),
                        OryxisColors::t().accent,
                    ),
                );
                let cancel_btn = self.settings_nav_slot(
                    crate::keynav::RowAction::activate(Message::Sync(SyncMessage::JoinPairingCancel)),
                    6.0,
                    styled_button(
                        crate::i18n::t("sync_pairing_cancel"),
                        Message::Sync(SyncMessage::JoinPairingCancel),
                        OryxisColors::t().button_bg,
                    ),
                );
                // The link input is built after the Connect and
                // Cancel pair so the keyboard order matches the
                // on-screen order.
                let link_input = self.settings_nav_slot(
                    crate::keynav::RowAction::input(iced::widget::Id::new(
                        "set-sync-pair-link",
                    )),
                    10.0,
                    text_input(
                        crate::i18n::t("sync_pairing_link_placeholder"),
                        &self.sync.pairing.join_link_input,
                    )
                    .id(iced::widget::Id::new("set-sync-pair-link"))
                    .on_input(|v| Message::Sync(SyncMessage::JoinLinkChanged(v)))
                    .padding(8)
                    .width(360)
                    .style(crate::widgets::rounded_input_style)
                    .align_x(dir_align_x())
                    .into(),
                );
                let link_connect_btn = self.settings_nav_slot(
                    crate::keynav::RowAction::activate(Message::Sync(SyncMessage::JoinPairingByLink)),
                    6.0,
                    styled_button(
                        crate::i18n::t("sync_pairing_connect_with_link"),
                        Message::Sync(SyncMessage::JoinPairingByLink),
                        OryxisColors::t().accent,
                    ),
                );
                pairing_section = pairing_section
                    .push(code_input)
                    .push(Space::new().height(8))
                    .push(target_input)
                    .push(Space::new().height(10))
                    .push(dir_row(vec![
                        connect_btn,
                        Space::new().width(8).into(),
                        cancel_btn,
                    ]))
                    .push(Space::new().height(14))
                    .push(text(crate::i18n::t("sync_pairing_or_separator"))
                        .size(11)
                        .color(OryxisColors::t().text_muted))
                    .push(Space::new().height(6))
                    .push(link_input)
                    .push(Space::new().height(8))
                    .push(link_connect_btn);
            }
        }

        // Inline status banner inside the pairing card. The
        // same `sync_status` field also shows under "Sync Now"
        // in the Options card, but when the user is actively
        // pairing they're looking here, so we mirror it
        // adjacent to the form they're filling in.
        if !matches!(self.sync.pairing.state, crate::state::SyncPairingState::Idle)
            && let Some(status) = &self.sync.status
        {
            pairing_section = pairing_section
                .push(Space::new().height(8))
                .push(text(status.as_str())
                    .size(11)
                    .color(OryxisColors::t().text_muted));
        }

        // Paired devices list. Empty until the first successful
        // pairing on either side; pre-Phase B builds never
        // populated this because the engine wasn't wired.
        if !self.sync.peers.is_empty() {
            pairing_section = pairing_section
                .push(Space::new().height(14))
                .push(text(crate::i18n::t("sync_paired_devices"))
                    .size(12)
                    .color(OryxisColors::t().text_secondary))
                .push(Space::new().height(6));
            for peer in &self.sync.peers {
                let last_sync = peer.last_synced_at
                    // Stored UTC; show in the user's local timezone.
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| crate::i18n::t("sync_never").into());
                let unpair = self.settings_nav_slot(
                    crate::keynav::RowAction::activate(
                        Message::Sync(SyncMessage::UnpairDevice(peer.peer_id)),
                    ),
                    4.0,
                    button(
                        text(crate::i18n::t("sync_unpair")).size(11).color(OryxisColors::t().error)
                    ).on_press(Message::Sync(SyncMessage::UnpairDevice(peer.peer_id))).style(|_, _| button::Style {
                        background: Some(Background::Color(Color::TRANSPARENT)),
                        ..Default::default()
                    })
                    .into(),
                );
                pairing_section = pairing_section.push(
                    dir_row(vec![
                        text(&peer.device_name).size(13).color(OryxisColors::t().text_primary).into(),
                        Space::new().width(Length::Fill).into(),
                        text(last_sync).size(11).color(OryxisColors::t().text_muted).into(),
                        Space::new().width(8).into(),
                        unpair,
                    ]).align_y(iced::Alignment::Center),
                ).push(Space::new().height(4));
            }
        }

        panel_section(
            device_col
                .push(Space::new().height(16))
                .push(pairing_section),
        )
    }

    /// "Set up your own relay" wizard: generates ready-to-paste
    /// server files for the self-hosted oryxis-relay and adopts
    /// the endpoint (signaling URL + token settings) once the
    /// reachability test passes. The in-app fast path for what
    /// SELF_HOSTING.md documents long-form.
    fn sync_relay_wizard_card(&self) -> iced::widget::Column<'_, Message> {
        let w = &self.sync.relay_wizard;
        let mut wizard_col: iced::widget::Column<'_, Message> =
            column![self.settings_nav_slot_labeled(
                t("sync_wizard_button"),
                crate::keynav::RowAction::activate(Message::Sync(SyncMessage::WizardToggle)),
                6.0,
                styled_button(
                    crate::i18n::t("sync_wizard_button"),
                    Message::Sync(SyncMessage::WizardToggle),
                    OryxisColors::t().button_bg,
                ),
            )];
        if w.open {
            wizard_col = wizard_col
                .push(Space::new().height(8))
                .push(
                    text(crate::i18n::t("sync_wizard_intro"))
                        .size(11)
                        .color(OryxisColors::t().text_muted),
                )
                .push(Space::new().height(10))
                .push(
                    text(crate::i18n::t("sync_wizard_domain"))
                        .size(12)
                        .color(OryxisColors::t().text_muted),
                )
                .push(Space::new().height(4))
                .push(self.settings_nav_slot(
                    crate::keynav::RowAction::input(iced::widget::Id::new(
                        "set-sync-wizard-domain",
                    )),
                    10.0,
                    text_input("relay.example.com", &w.domain)
                        .id(iced::widget::Id::new("set-sync-wizard-domain"))
                        .on_input(|v| Message::Sync(SyncMessage::WizardDomainChanged(v)))
                        .padding(8)
                        .width(320)
                        .style(crate::widgets::rounded_input_style)
                        .align_x(dir_align_x())
                        .into(),
                ))
                .push(Space::new().height(8))
                .push(
                    text(crate::i18n::t("sync_wizard_public_port"))
                        .size(12)
                        .color(OryxisColors::t().text_muted),
                )
                .push(Space::new().height(4))
                .push(self.settings_nav_slot(
                    crate::keynav::RowAction::input(iced::widget::Id::new(
                        "set-sync-wizard-port",
                    )),
                    10.0,
                    text_input("443", &w.port)
                        .id(iced::widget::Id::new("set-sync-wizard-port"))
                        .on_input(|v| Message::Sync(SyncMessage::WizardPortChanged(v)))
                        .padding(8)
                        .width(120)
                        .style(crate::widgets::rounded_input_style)
                        .align_x(dir_align_x())
                        .into(),
                ))
                .push(Space::new().height(8))
                .push(
                    text(format!(
                        "{}: {}",
                        crate::i18n::t("sync_wizard_token"),
                        w.token,
                    ))
                    .size(11)
                    .color(OryxisColors::t().text_muted),
                )
                .push(Space::new().height(6))
                .push(self.settings_nav_slot(
                    crate::keynav::RowAction::activate(
                        Message::Sync(SyncMessage::WizardRegenToken),
                    ),
                    6.0,
                    styled_button(
                        crate::i18n::t("sync_wizard_regen"),
                        Message::Sync(SyncMessage::WizardRegenToken),
                        OryxisColors::t().button_bg,
                    ),
                ));
            // Artifact format selector: three plain buttons (the
            // labels are proper nouns, identical in every locale),
            // accent marks the selected one.
            let fmt_btn = |label: &'static str,
                           fmt: crate::state::RelayWizardFormat|
             -> Element<'_, Message> {
                let selected = w.format == fmt;
                self.settings_nav_slot(
                    crate::keynav::RowAction::activate(
                        Message::Sync(SyncMessage::WizardFormatChanged(fmt)),
                    ),
                    6.0,
                    styled_button(
                        label,
                        Message::Sync(SyncMessage::WizardFormatChanged(fmt)),
                        if selected {
                            OryxisColors::t().accent
                        } else {
                            OryxisColors::t().button_bg
                        },
                    ),
                )
            };
            wizard_col = wizard_col.push(Space::new().height(12)).push(dir_row(vec![
                fmt_btn("Docker Compose", crate::state::RelayWizardFormat::Compose),
                Space::new().width(8).into(),
                fmt_btn("systemd", crate::state::RelayWizardFormat::Systemd),
                Space::new().width(8).into(),
                fmt_btn("Caddy", crate::state::RelayWizardFormat::Caddy),
            ]));
            let artifact = w.artifact();
            wizard_col = wizard_col
                .push(Space::new().height(10))
                .push(
                    text(crate::i18n::t("sync_wizard_files"))
                        .size(12)
                        .color(OryxisColors::t().text_secondary),
                )
                .push(Space::new().height(4))
                .push(
                    container(
                        text(artifact.clone())
                            .size(10)
                            .font(iced::Font::MONOSPACE)
                            .color(OryxisColors::t().text_secondary),
                    )
                    .width(Length::Fill)
                    .padding(10)
                    .style(|_| container::Style {
                        background: Some(Background::Color(
                            OryxisColors::t().bg_surface,
                        )),
                        border: Border {
                            radius: Radius::from(6.0),
                            color: OryxisColors::t().border,
                            width: 1.0,
                        },
                        ..Default::default()
                    }),
                )
                .push(Space::new().height(6))
                .push(self.settings_nav_slot(
                    crate::keynav::RowAction::activate(
                        Message::CopyToClipboard(artifact.clone()),
                    ),
                    6.0,
                    styled_button(
                        crate::i18n::t("terminal_copy"),
                        Message::CopyToClipboard(artifact),
                        OryxisColors::t().button_bg,
                    ),
                ))
                .push(Space::new().height(10))
                .push(
                    text(crate::i18n::t("sync_wizard_steps"))
                        .size(11)
                        .color(OryxisColors::t().text_muted),
                )
                .push(Space::new().height(10));
            // Test button only fires with a domain typed and no
            // probe in flight; recorded only when enabled so
            // keyboard Enter can't double-fire a probe.
            let test_msg = (!w.testing && w.base_url().is_some())
                .then_some(Message::Sync(SyncMessage::WizardTest));
            let test_btn = styled_button_opt(
                crate::i18n::t("sync_wizard_test"),
                test_msg.clone(),
                OryxisColors::t().accent,
            );
            let test_btn: Element<'_, Message> = if let Some(m) = test_msg {
                self.settings_nav_slot(
                    crate::keynav::RowAction::activate(m),
                    6.0,
                    test_btn,
                )
            } else {
                test_btn
            };
            wizard_col = wizard_col.push(test_btn);
            if let Some(result) = &w.result {
                let (txt, color) = match result {
                    Ok(()) => (
                        crate::i18n::t("sync_wizard_test_ok").to_string(),
                        OryxisColors::t().text_muted,
                    ),
                    Err(e) => (
                        format!(
                            "{}: {e}",
                            crate::i18n::t("sync_wizard_test_err"),
                        ),
                        OryxisColors::t().error,
                    ),
                };
                wizard_col = wizard_col
                    .push(Space::new().height(8))
                    .push(text(txt).size(11).color(color));
            }
        }
        wizard_col
    }

    /// Advanced card (P2P only): the signaling URL / token, relay URL
    /// and listen-port inputs, followed by the relay wizard.
    fn sync_advanced_card(&self) -> Element<'_, Message> {
        // Advanced
        let signaling_input = self.settings_nav_slot_labeled(
            t("sync_signaling_url"),
            crate::keynav::RowAction::input(iced::widget::Id::new(
                "set-sync-signaling-url",
            )),
            10.0,
            text_input("https://...", &self.sync.signaling_url)
                .id(iced::widget::Id::new("set-sync-signaling-url"))
                .on_input(|v| Message::Sync(SyncMessage::SignalingUrlChanged(v)))
                .padding(8)
                .width(300)
                .style(crate::widgets::rounded_input_style).align_x(dir_align_x())
                .into(),
        );
        // Keyboard rows (audit fix: this field recorded
        // nothing): the field, then its reveal eye.
        let signaling_token_idx = self
            .settings_nav_record_labeled(
                t("sync_signaling_token"),
                crate::keynav::RowAction::input(
                    iced::widget::Id::new("set-sync-signaling-token"),
                ),
            );
        let signaling_token_input = self.settings_nav_ring_at(
            signaling_token_idx,
            10.0,
            container(
                crate::widgets::password_input_with_eye_nav(
                    crate::i18n::t("sync_signaling_token_placeholder"),
                    &self.sync.signaling_token,
                    |v| Message::Sync(SyncMessage::SignalingTokenChanged(v.into())),
                    None,
                    self.revealed_secrets
                        .contains(&crate::state::SecretField::SyncSignalingToken),
                    Message::Settings(SettingsMessage::ToggleSecretVisibility(
                        crate::state::SecretField::SyncSignalingToken,
                    )),
                    8.0,
                    Some(iced::widget::Id::new("set-sync-signaling-token")),
                    |eye| self.settings_nav_slot(
                        crate::keynav::RowAction::activate(
                            Message::Settings(SettingsMessage::ToggleSecretVisibility(
                                crate::state::SecretField::SyncSignalingToken,
                            )),
                        ),
                        6.0,
                        eye,
                    ),
                ),
            )
            .width(300)
            .into(),
        );
        let relay_input = self.settings_nav_slot_labeled(
            t("sync_relay_url"),
            crate::keynav::RowAction::input(iced::widget::Id::new("set-sync-relay-url")),
            10.0,
            text_input(crate::i18n::t("sync_relay_optional"), &self.sync.relay_url)
                .id(iced::widget::Id::new("set-sync-relay-url"))
                .on_input(|v| Message::Sync(SyncMessage::RelayUrlChanged(v)))
                .padding(8)
                .width(300)
                .style(crate::widgets::rounded_input_style).align_x(dir_align_x())
                .into(),
        );
        let port_input = self.settings_nav_slot_labeled(
            t("sync_listen_port"),
            crate::keynav::RowAction::input(iced::widget::Id::new(
                "set-sync-listen-port",
            )),
            10.0,
            text_input("0", &self.sync.listen_port)
                .id(iced::widget::Id::new("set-sync-listen-port"))
                .on_input(|v| Message::Sync(SyncMessage::ListenPortChanged(v)))
                .padding(8)
                .width(100)
                .style(crate::widgets::rounded_input_style).align_x(dir_align_x())
                .into(),
        );

        let advanced_col = column![
            text(crate::i18n::t("sync_signaling_url")).size(12).color(OryxisColors::t().text_muted),
            Space::new().height(4),
            signaling_input,
            Space::new().height(8),
            text(crate::i18n::t("sync_signaling_token")).size(12).color(OryxisColors::t().text_muted),
            Space::new().height(4),
            signaling_token_input,
            Space::new().height(8),
            text(crate::i18n::t("sync_relay_url")).size(12).color(OryxisColors::t().text_muted),
            Space::new().height(4),
            relay_input,
            Space::new().height(8),
            text(crate::i18n::t("sync_listen_port")).size(12).color(OryxisColors::t().text_muted),
            Space::new().height(4),
            port_input,
            Space::new().height(4),
            text(crate::i18n::t("sync_listen_port_hint")).size(11).color(OryxisColors::t().text_muted),
        ];

        panel_section(
            advanced_col
                .push(Space::new().height(16))
                .push(self.sync_relay_wizard_card()),
        )
    }

    pub(crate) fn view_settings_sync(&self) -> Element<'_, Message> {
        // Keyboard rows are recorded in visual order. Recording happens
        // at construction, so every card below is built in its
        // on-screen position and the enabled-only cards are built
        // inside the `sync.enabled` branch. Do not reorder the card
        // calls.
        self.keynav_settings_reset();

        let is_sftp = self.sync.transport == "sftp";
        // Pairing, LAN discovery and the signaling / relay / port
        // fields are the P2P engine's own surface. Asking `!is_sftp`
        // put all of it in front of folder and Git users, who have no
        // engine to pair with.
        let is_p2p = self.sync_uses_p2p();

        let enable_card = self.sync_engine_state_card(is_p2p);

        // Plain-language primer: what sync is, that it's optional and
        // LAN-only by default (no Oryxis server), and what the user
        // must set up to sync across networks. Answers the recurring
        // "is sync required / where does my data go?" question.
        let how_section = panel_section(column![
            text(crate::i18n::t("sync_how_body"))
                .size(12)
                .color(OryxisColors::t().text_secondary),
        ]);

        let mut content_col: iced::widget::Column<'_, Message> = column![
            enable_card,
        ]
        .width(Length::Fill)
        .align_x(dir_align_x());

        content_col = content_col
            .push(Space::new().height(18))
            .push(crate::widgets::settings_group_header(crate::i18n::t("sync_how_title")))
            .push(Space::new().height(8))
            .push(how_section);

        if self.sync.enabled {
            let transport_section = self.sync_transport_card(is_sftp);
            let options_section = self.sync_options_card(is_sftp, is_p2p);

            content_col = content_col
                .push(Space::new().height(18))
                .push(crate::widgets::settings_group_header(crate::i18n::t("sync_transport")))
                .push(Space::new().height(8))
                .push(transport_section)
                .push(Space::new().height(18))
                .push(crate::widgets::settings_group_header(crate::i18n::t("sync_options")))
                .push(Space::new().height(8))
                .push(options_section);

            if is_p2p {
                let devices_card = self.sync_devices_card();
                let advanced_card = self.sync_advanced_card();

                content_col = content_col
                    .push(Space::new().height(18))
                    .push(crate::widgets::settings_group_header(crate::i18n::t("sync_device")))
                    .push(Space::new().height(8))
                    .push(devices_card)
                    .push(Space::new().height(18))
                    .push(crate::widgets::settings_group_header(crate::i18n::t("sync_advanced")))
                    .push(Space::new().height(8))
                    .push(advanced_card);
            }
        }
        content_col = content_col.push(Space::new().height(24));

        scrollable(
            container(content_col)
                .padding(Padding { top: 24.0, right: 24.0, bottom: 24.0, left: 24.0 }),
        )
        // Stable id so the keyboard router can keep the selected row
        // in view.
        .id(iced::widget::Id::new("settings-sync-scroll"))
        .on_scroll(|v| Message::Settings(SettingsMessage::SectionScrolled(v.relative_offset().y)))
        .height(Length::Fill)
        .into()
    }
}

/// Whether this WebDAV URL would send the account password in the
/// clear. `http://` to anywhere but the loopback address does; `https`,
/// an empty field and a still-being-typed prefix do not.
fn webdav_is_cleartext(url: &str) -> bool {
    let url = url.trim();
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    // Userinfo first: `http://me@nas.local/` must not read as host `me`.
    let hostport = authority.rsplit('@').next().unwrap_or("");
    // A bracketed IPv6 literal carries colons of its own, so the port
    // is only whatever follows the closing bracket.
    let host = match hostport.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => hostport.split(':').next().unwrap_or(""),
    };
    !matches!(host, "localhost" | "127.0.0.1" | "::1")
}

#[cfg(test)]
mod webdav_url_tests {
    use super::webdav_is_cleartext;

    #[test]
    fn https_and_loopback_are_quiet() {
        assert!(!webdav_is_cleartext("https://cloud.example/dav/"));
        assert!(!webdav_is_cleartext("http://localhost:8080/dav/"));
        assert!(!webdav_is_cleartext("http://127.0.0.1:8088/oryxis/"));
        assert!(!webdav_is_cleartext("http://[::1]:8080/dav/"));
        // Nothing typed yet is not a warning either.
        assert!(!webdav_is_cleartext(""));
        assert!(!webdav_is_cleartext("http"));
    }

    #[test]
    fn plain_http_to_a_real_host_warns() {
        assert!(webdav_is_cleartext("http://nas.local/dav/"));
        assert!(webdav_is_cleartext("http://192.168.1.10:5005/dav/"));
        assert!(webdav_is_cleartext("  http://cloud.example/dav/  "));
        // Userinfo must not be mistaken for the host.
        assert!(webdav_is_cleartext("http://me@nas.local/dav/"));
    }
}

//! Settings -> About section view. Split out of views/settings/mod.rs.

use super::*;
use iced::widget::column;

impl Oryxis {
    pub(crate) fn view_settings_about(&self) -> Element<'_, Message> {
        // Keyboard rows are recorded in visual order.
        self.keynav_settings_reset();
        // Channel-aware build string: nightly builds append the
        // channel + short commit so a nightly user sees exactly what
        // they're running, not just the base version number.
        let version_str = match crate::update::build_channel() {
            crate::update::UpdateChannel::Nightly => format!(
                "Oryxis v{} nightly ({})",
                env!("CARGO_PKG_VERSION"),
                env!("ORYXIS_GIT_SHA").chars().take(7).collect::<String>(),
            ),
            crate::update::UpdateChannel::Stable => {
                format!("Oryxis v{}", env!("CARGO_PKG_VERSION"))
            }
        };
        // Logo beside the name + tagline, like the lock screen.
        let about_header = dir_row(vec![
            iced::widget::svg(self.logo_handle.clone())
                .width(Length::Fixed(48.0))
                .height(Length::Fixed(48.0))
                .into(),
            Space::new().width(14).into(),
            column![
                text(version_str).size(16).color(OryxisColors::t().text_primary),
                Space::new().height(4),
                text(t("app_tagline")).size(13).color(OryxisColors::t().text_secondary),
            ]
            .align_x(dir_align_x())
            .into(),
        ])
        .align_y(iced::Alignment::Center);
        let about_section = panel_section(column![
            about_header,
            Space::new().height(16),
            settings_row(t("built_with"), "Iced, russh, alacritty_terminal".into()),
            Space::new().height(6),
            settings_row(t("license"), "AGPL-3.0".into()),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::OpenUrl(
                    "https://oryxis.app/".to_string(),
                )),
                8.0,
                crate::widgets::settings_row_link(
                    crate::i18n::t("website"),
                    "oryxis.app".into(),
                    "https://oryxis.app/".into(),
                ),
            ),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::OpenUrl(
                    "https://github.com/wilsonglasser/oryxis".to_string(),
                )),
                8.0,
                crate::widgets::settings_row_link(
                    crate::i18n::t("github"),
                    "github.com/wilsonglasser/oryxis".into(),
                    "https://github.com/wilsonglasser/oryxis".into(),
                ),
            ),
        ]);

        // The update panel renders between About and the vault stats,
        // so it is BUILT here (before the stats) to keep the keyboard
        // recording in visual order.
        let auto_update_section = self.view_settings_update_panel();

        // Each stat row navigates to its section (issue #38):
        // the count doubles as a shortcut into the data it
        // describes. Two rows are composites, each summing exactly
        // what its destination view lists: Logs combines connection
        // events + session recordings, and Keychain combines SSH keys
        // + identities (issue #148: counting keys alone reported 0 on
        // a vault whose keychain held only identities). Proxy
        // identities stay out; they are the Proxies view.
        let vault_section = panel_section(column![
            text(crate::i18n::t("vault_stats")).size(13).color(OryxisColors::t().text_primary),
            Space::new().height(8),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::Navigation(NavigationMessage::ChangeView(
                    crate::state::View::Dashboard,
                ))),
                8.0,
                crate::widgets::settings_row_nav(
                    crate::i18n::t("hosts"),
                    self.connections.len().to_string(),
                    Message::Navigation(NavigationMessage::ChangeView(crate::state::View::Dashboard)),
                ),
            ),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::Navigation(NavigationMessage::ChangeView(
                    crate::state::View::Keys,
                ))),
                8.0,
                crate::widgets::settings_row_nav(
                    crate::i18n::t("keychain"),
                    (self.keys.len() + self.identities.len()).to_string(),
                    Message::Navigation(NavigationMessage::ChangeView(crate::state::View::Keys)),
                ),
            ),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::Navigation(NavigationMessage::ChangeView(
                    crate::state::View::Snippets,
                ))),
                8.0,
                crate::widgets::settings_row_nav(
                    crate::i18n::t("snippets"),
                    self.snippets.len().to_string(),
                    Message::Navigation(NavigationMessage::ChangeView(crate::state::View::Snippets)),
                ),
            ),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::Navigation(NavigationMessage::ChangeView(
                    crate::state::View::Dashboard,
                ))),
                8.0,
                crate::widgets::settings_row_nav(
                    t("groups"),
                    self.groups.len().to_string(),
                    Message::Navigation(NavigationMessage::ChangeView(crate::state::View::Dashboard)),
                ),
            ),
            Space::new().height(6),
            self.settings_nav_slot(
                crate::keynav::RowAction::activate(Message::Navigation(NavigationMessage::ChangeView(
                    crate::state::View::History,
                ))),
                8.0,
                crate::widgets::settings_row_nav(
                    t("logs"),
                    (self.logs_total + self.session_logs_total).to_string(),
                    Message::Navigation(NavigationMessage::ChangeView(crate::state::View::History)),
                ),
            ),
        ]);

        scrollable(
            container(
                column![
                    about_section,
                    Space::new().height(12),
                    auto_update_section,
                    Space::new().height(12),
                    vault_section,
                    Space::new().height(24),
                ]
                .width(Length::Fill)
                .align_x(dir_align_x()),
            )
            .padding(Padding { top: 24.0, right: 24.0, bottom: 24.0, left: 24.0 }),
        )
        // Stable id so the keyboard router can keep the selected row
        // in view.
        .id(iced::widget::Id::new("settings-about-scroll"))
        .on_scroll(|v| Message::Settings(SettingsMessage::SectionScrolled(v.relative_offset().y)))
        .height(Length::Fill)
        .into()
    }

    /// Settings -> About: the self-update panel (auto-check toggle,
    /// release channel, "check now" + status line).
    ///
    /// Empty inside an MSIX package: the Microsoft Store services the
    /// app, `WindowsApps` is read-only, and our installer would only lay
    /// down a second unpackaged copy. Returning early also keeps the
    /// panel's keynav rows OUT of the recording, so the keyboard walk
    /// never stops on controls that aren't on screen.
    pub(crate) fn view_settings_update_panel(&self) -> Element<'_, Message> {
        if crate::packaged::is_packaged() {
            return Space::new().into();
        }
        let auto_update_enabled = self.prefs.auto_check_updates;
        let auto_update_toggle = self.nav_toggle_row(
            crate::i18n::t("auto_check_updates"),
            auto_update_enabled,
            Message::Update(UpdateMessage::SettingToggleAutoCheckUpdates),
        );
        // Non-standard picker layout (label line above the control),
        // so it takes the cycle_pair + slot treatment instead of
        // nav_pick_row.
        let (channel_prev, channel_next) = crate::keynav::slots::cycle_pair(
            &crate::update::UPDATE_CHANNELS,
            &self.prefs.update_channel,
            |v| Message::Update(UpdateMessage::SettingUpdateChannelChanged(v)),
        );
        let channel_picker = self.settings_nav_slot_labeled(
            t("update_channel"),
            crate::keynav::RowAction::picker(channel_prev, channel_next),
            10.0,
            pick_list(
                Some(self.prefs.update_channel),
                crate::update::UPDATE_CHANNELS.to_vec(),
                |c: &crate::update::UpdateChannel| match c {
                    crate::update::UpdateChannel::Stable => t("update_channel_stable").to_string(),
                    crate::update::UpdateChannel::Nightly => t("update_channel_nightly").to_string(),
                },
            )
            .on_select(|v| Message::Update(UpdateMessage::SettingUpdateChannelChanged(v)))
            .on_open(Message::Navigation(NavigationMessage::PickOpenChanged(true)))
            .on_close(Message::Navigation(NavigationMessage::PickOpenChanged(false)))
            .width(260)
            .padding(10)
            .style(crate::widgets::rounded_pick_list_style)
            .into(),
        );
        let check_now_btn = self.settings_nav_slot_labeled(
            t("check_for_updates_now"),
            crate::keynav::RowAction::activate(Message::Update(UpdateMessage::CheckForUpdateManual)),
            6.0,
            styled_button(
                t("check_for_updates_now"),
                Message::Update(UpdateMessage::CheckForUpdateManual),
                OryxisColors::t().accent,
            ),
        );
        // Offline mode is said here whether or not a check was asked
        // for: the row above still offers one, and this is where the
        // answer to "why has it not run" belongs.
        let status = if self.prefs.offline_mode {
            Some(crate::update::UpdateStatus::Offline)
        } else {
            self.update_check_status.clone()
        };
        let status_line: Element<'_, Message> = match &status {
            Some(status) => {
                use crate::update::UpdateStatus;
                let (msg, color) = match status {
                    UpdateStatus::Checking => (
                        t("update_check_checking").to_string(),
                        OryxisColors::t().text_muted,
                    ),
                    UpdateStatus::Offline => (
                        t("update_check_offline").to_string(),
                        OryxisColors::t().warning,
                    ),
                    UpdateStatus::UpToDate => (
                        format!(
                            "{} ({})",
                            t("update_check_up_to_date"),
                            env!("CARGO_PKG_VERSION"),
                        ),
                        OryxisColors::t().success,
                    ),
                    UpdateStatus::Failed(cause) => (
                        format!("{}: {}", t("update_check_failed"), cause),
                        OryxisColors::t().error,
                    ),
                };
                // Failures get an inline Retry next to the cause so
                // the user doesn't have to hunt for the check button.
                let mut line_items: Vec<Element<'_, Message>> =
                    vec![text(msg).size(11).color(color).into()];
                if matches!(status, UpdateStatus::Failed(_)) {
                    line_items.push(Space::new().width(10).into());
                    line_items.push(self.settings_nav_slot(
                        crate::keynav::RowAction::activate(Message::Update(UpdateMessage::CheckForUpdateManual)),
                        6.0,
                        styled_button(
                            t("retry"),
                            Message::Update(UpdateMessage::CheckForUpdateManual),
                            OryxisColors::t().text_muted,
                        ),
                    ));
                }
                let line = crate::widgets::dir_row(line_items)
                    .align_y(iced::Alignment::Center);
                container(line)
                    .padding(Padding { top: 8.0, right: 0.0, bottom: 0.0, left: 0.0 })
                    .into()
            }
            None => Space::new().into(),
        };
        // A downloaded update waiting for its restart (the ask was
        // declined while sessions were live): the restart stays one
        // click away here, so the download is not lost with the modal.
        let ready_line: Element<'_, Message> = match &self.update_ready {
            Some(ready) => {
                let line = crate::widgets::dir_row(vec![
                    text(t("update_ready").replacen("{new}", &ready.info.version, 1))
                        .size(11)
                        .color(OryxisColors::t().success)
                        .into(),
                    Space::new().width(10).into(),
                    self.settings_nav_slot(
                        crate::keynav::RowAction::activate(Message::Update(UpdateMessage::UpdateInstallNow)),
                        6.0,
                        styled_button(
                            t("update_restart_now"),
                            Message::Update(UpdateMessage::UpdateInstallNow),
                            OryxisColors::t().accent,
                        ),
                    ),
                ])
                .align_y(iced::Alignment::Center);
                container(line)
                    .padding(Padding { top: 8.0, right: 0.0, bottom: 0.0, left: 0.0 })
                    .into()
            }
            None => Space::new().into(),
        };
        // Bleeding-edge warning, only while the nightly channel is
        // selected, so stable users don't see scary copy.
        let channel_note: Element<'_, Message> =
            if self.prefs.update_channel == crate::update::UpdateChannel::Nightly {
                container(
                    text(t("update_channel_nightly_warning"))
                        .size(11)
                        .color(OryxisColors::t().text_muted),
                )
                .padding(Padding { top: 4.0, right: 0.0, bottom: 0.0, left: 0.0 })
                .into()
            } else {
                Space::new().into()
            };
        panel_section(column![
            auto_update_toggle,
            Space::new().height(4),
            text(t("setting_update_check_desc"))
                .size(11).color(OryxisColors::t().text_muted),
            Space::new().height(12),
            text(t("update_channel")).size(12).color(OryxisColors::t().text_secondary),
            Space::new().height(4),
            channel_picker,
            channel_note,
            Space::new().height(10),
            check_now_btn,
            status_line,
            ready_line,
        ])
    }
}

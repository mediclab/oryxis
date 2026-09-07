//! Update-available modal: three choices (skip / later / update now) + a
//! short release-notes preview. During download we swap the action row
//! for a progress bar; errors stay inline in the same card. Once the
//! download is in hand the row becomes the ask to restart.
//!
//! The card is `Modal::UpdateOffer` on the keyboard layer: its rows are
//! recorded in visual order (the release link, then the buttons), with
//! Later as the default row in every state, because the card opens
//! itself (the boot check) and a stray Enter must neither start a
//! download nor restart the app.

use iced::border::Radius;
use iced::widget::{column, container, scrollable, text, MouseArea, Space};
use iced::{Background, Border, Element, Length, Padding};

use crate::app::{UpdateMessage, Message, Oryxis};
use crate::i18n::t;
use crate::keynav::RowAction;
use crate::theme::OryxisColors;
use crate::widgets::{dir_row, styled_button};

impl Oryxis {
    pub(crate) fn view_update_modal(&self) -> Element<'_, Message> {
        let info = match &self.pending_update {
            Some(i) => i,
            None => return Space::new().into(),
        };
        // The topmost navigable surface of the frame: record from a
        // clean ring, like every other modal view.
        self.modal_nav_reset();

        // What this binary actually is. A nightly build carries the same
        // CARGO_PKG_VERSION as the stable it branched from, so showing the
        // bare version when a nightly user is offered a stable reads as
        // "0.8.3 is available. You're on 0.8.3." Identify nightlies by
        // channel + embedded commit instead.
        let current = match crate::update::build_channel() {
            crate::update::UpdateChannel::Nightly => {
                let sha = env!("ORYXIS_GIT_SHA");
                if sha == "unknown" {
                    "nightly".to_string()
                } else {
                    let short: String = sha.chars().take(8).collect();
                    format!("nightly ({short})")
                }
            }
            crate::update::UpdateChannel::Stable => env!("CARGO_PKG_VERSION").to_string(),
        };
        let title = text(t("update_available")).size(18).font(iced::Font {
            weight: iced::font::Weight::Bold,
            ..iced::Font::new(crate::theme::SYSTEM_UI_FAMILY)
        }).color(OryxisColors::t().text_primary);

        let subtitle = text(
            t("update_subtitle")
                .replacen("{new}", &info.version, 1)
                .replacen("{current}", &current, 1),
        )
        .size(12)
        .color(OryxisColors::t().text_secondary);

        // Release notes preview, first ~40 lines, in a scrollable box so
        // long changelogs don't bloat the modal. Rendered as plain text
        // (we're not doing markdown rendering here).
        let notes_preview: String = info
            .body
            .lines()
            .take(40)
            .collect::<Vec<_>>()
            .join("\n");
        let notes: Element<'_, Message> = if notes_preview.trim().is_empty() {
            Space::new().into()
        } else {
            container(
                scrollable(
                    text(notes_preview)
                        .size(11)
                        .color(OryxisColors::t().text_muted)
                        .font(iced::Font::MONOSPACE),
                )
                .height(Length::Fixed(160.0)),
            )
            .padding(12)
            .width(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(OryxisColors::t().bg_surface)),
                border: Border { radius: Radius::from(8.0), color: OryxisColors::t().border, width: 1.0 },
                ..Default::default()
            })
            .into()
        };

        let release_link = self.modal_nav_slot(
            RowAction::activate(Message::Update(UpdateMessage::UpdateOpenRelease)),
            4.0,
            false,
            MouseArea::new(
                text(t("open_release_github"))
                    .size(11)
                    .color(OryxisColors::t().accent),
            )
            .on_press(Message::Update(UpdateMessage::UpdateOpenRelease))
            .into(),
        );

        // Action row OR progress bar depending on state.
        let action_area: Element<'_, Message> = if self.update_downloading {
            let pct = (self.update_progress * 100.0).clamp(0.0, 100.0) as u32;
            // Both ends of this bar used to misdraw: a weightless
            // `FillPortion(0)` takes the WHOLE track in iced rather than
            // vanishing, so 0% painted a full bar and 100% handed the track
            // back to the empty sibling. `progress_track` omits the
            // weightless side instead (same defect as issue #107).
            let bar = crate::widgets::progress_track(
                self.update_progress,
                6.0,
                OryxisColors::t().accent,
                OryxisColors::t().bg_hover,
            );
            // Branch on the update's own artifact kind, not the channel
            // setting: a nightly binary can legitimately download a stable
            // installer (channel flipped back to Stable), and the setting
            // can lag the offer that's actually on screen.
            let label = match info.artifact {
                crate::update::UpdateArtifact::Installer => t("downloading_installer"),
                // Everything applied in place (nightly binary, portable
                // zip, AppImage) downloads "the update", not an installer.
                crate::update::UpdateArtifact::Binary
                | crate::update::UpdateArtifact::PortableArchive
                | crate::update::UpdateArtifact::AppImage => t("downloading_update"),
            };
            column![
                text(format!("{} {}%", label, pct))
                    .size(11).color(OryxisColors::t().text_muted),
                Space::new().height(8),
                bar,
            ]
            .into()
        } else if self
            .update_ready
            .as_ref()
            .is_some_and(|r| r.info.version == info.version)
        {
            // Downloaded and waiting: installing means restarting, and
            // with live sessions open that is the ask itself, in the
            // surface that is already up. Declining keeps the download.
            let live = self.live_session_tab_count();
            let mut lines = column![
                text(t("update_ready").replacen("{new}", &info.version, 1))
                    .size(12)
                    .color(OryxisColors::t().text_primary),
            ];
            if live > 0 {
                lines = lines.push(Space::new().height(4)).push(
                    text(t("update_ready_sessions").replacen("{n}", &live.to_string(), 1))
                        .size(11)
                        .color(OryxisColors::t().warning),
                );
            }
            column![
                lines,
                Space::new().height(12),
                dir_row(vec![
                    Space::new().width(Length::Fill).into(),
                    // Later is the default row: the restart closes every
                    // live session, which is the very thing this state
                    // exists to ask about.
                    self.modal_nav_slot_default(
                        RowAction::activate(Message::Update(UpdateMessage::UpdateLater)),
                        6.0,
                        false,
                        styled_button(
                            t("update_later"),
                            Message::Update(UpdateMessage::UpdateLater),
                            OryxisColors::t().bg_hover,
                        ),
                    ),
                    Space::new().width(8).into(),
                    self.modal_nav_slot(
                        RowAction::activate(Message::Update(UpdateMessage::UpdateInstallNow)),
                        6.0,
                        true,
                        styled_button(
                            t("update_restart_now"),
                            Message::Update(UpdateMessage::UpdateInstallNow),
                            OryxisColors::t().accent,
                        ),
                    ),
                ])
                .align_y(iced::Alignment::Center),
            ]
            .into()
        } else {
            dir_row(vec![
                self.modal_nav_slot(
                    RowAction::activate(Message::Update(UpdateMessage::UpdateSkipVersion)),
                    6.0,
                    false,
                    styled_button(
                        t("update_skip_version"),
                        Message::Update(UpdateMessage::UpdateSkipVersion),
                        OryxisColors::t().bg_selected,
                    ),
                ),
                Space::new().width(Length::Fill).into(),
                // Later is the default row here too: the offer opened
                // itself, and Enter on a dialog nobody asked for must
                // commit to nothing.
                self.modal_nav_slot_default(
                    RowAction::activate(Message::Update(UpdateMessage::UpdateLater)),
                    6.0,
                    false,
                    styled_button(
                        t("update_later"),
                        Message::Update(UpdateMessage::UpdateLater),
                        OryxisColors::t().bg_hover,
                    ),
                ),
                Space::new().width(8).into(),
                self.modal_nav_slot(
                    RowAction::activate(Message::Update(UpdateMessage::UpdateStartDownload)),
                    6.0,
                    true,
                    styled_button(
                        t("update_now"),
                        Message::Update(UpdateMessage::UpdateStartDownload),
                        OryxisColors::t().accent,
                    ),
                ),
            ])
            .align_y(iced::Alignment::Center)
            .into()
        };

        let error_line: Element<'_, Message> = if let Some(err) = &self.update_error {
            container(
                text(format!("{}: {}", t("error"), err))
                    .size(11)
                    .color(OryxisColors::t().error),
            )
            .padding(Padding { top: 8.0, right: 0.0, bottom: 0.0, left: 0.0 })
            .into()
        } else {
            Space::new().into()
        };

        let body = container(
            column![
                title,
                Space::new().height(6),
                subtitle,
                Space::new().height(16),
                notes,
                Space::new().height(8),
                release_link,
                Space::new().height(16),
                action_area,
                error_line,
            ],
        )
        .padding(24)
        .width(Length::Fixed(520.0))
        .style(|_| container::Style {
            background: Some(Background::Color(OryxisColors::t().bg_primary)),
            border: Border {
                radius: Radius::from(12.0),
                color: OryxisColors::t().border,
                width: 1.0,
            },
            ..Default::default()
        });

        // Return the bare card; `widgets::modal_overlay` (the caller) owns
        // centering, the absorbing scrim, and the click-trap.
        body.into()
    }
}


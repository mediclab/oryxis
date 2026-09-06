//! In-app session player surface (issue #71), rendered by
//! `view_history` while `Oryxis.session_player` is `Some`: the
//! recording replays through the regular terminal widget pinned to its
//! recorded geometry, rendered at a font size fitted to the stage
//! (issue #89), under a transport bar (play/pause, restart, scrubber,
//! speed). Read-only by construction: the backend has no PTY and no
//! input callback is wired.

use std::sync::Arc;

use iced::border::Radius;
use iced::widget::button::Status as BtnStatus;
use iced::widget::{button, canvas, column, container, scrollable, text, Space};
use iced::{Background, Border, Color, Element, Length, Padding};

use oryxis_terminal::widget::{ChordResolver, TerminalChordAction, TerminalView};

use crate::app::{HistoryMessage, PlayerMessage, Message, Oryxis};
use crate::state::SessionPlayer;
use crate::theme::OryxisColors;

/// The user's terminal chords, minus the ones a recording cannot honour.
/// Copy, Select All and the scrollback paging all read the buffer, so
/// they apply unchanged; pasting the PRIMARY selection would consume the
/// highlight and hand the text to a backend that has no PTY, so that one
/// chord is declined instead of half-working.
fn replay_chords(base: ChordResolver) -> ChordResolver {
    Box::new(move |key, modifiers| match base(key, modifiers) {
        Some(TerminalChordAction::PasteSelection) => None,
        other => other,
    })
}

impl Oryxis {
    pub(crate) fn view_session_player<'a>(
        &'a self,
        p: &'a SessionPlayer,
    ) -> Element<'a, Message> {
        // Privacy Mode resolves like the static viewer: the recording's
        // host override wins, a deleted host falls back to the global
        // default; the toolbar Reveal toggle lifts the masking.
        let conn = self
            .session_logs
            .iter()
            .find(|e| e.id == p.log_id)
            .and_then(|e| self.connections.iter().find(|c| c.id == e.connection_id));
        let privacy_applies = conn
            .map(|c| self.privacy_active(c))
            .unwrap_or_else(|| self.privacy_global_active());
        let mask = privacy_applies && !self.privacy.revealed;

        // ── Header: title, geometry chip, reveal, close ──
        let title = if mask {
            crate::widgets::mask_blocks(&p.label)
        } else {
            p.label.clone()
        };
        let mut header_items: Vec<Element<'_, Message>> = vec![
            text(title)
                .size(16)
                .color(OryxisColors::t().text_primary)
                .into(),
            Space::new().width(10).into(),
            text(format!("{}x{}", p.cols, p.rows))
                .size(11)
                .color(OryxisColors::t().text_muted)
                .into(),
            Space::new().width(Length::Fill).into(),
        ];
        if privacy_applies {
            header_items.push(crate::widgets::privacy_reveal_btn(self.privacy.revealed));
            header_items.push(Space::new().width(8).into());
        }
        // Recording actions, mirroring the static viewer's header: a
        // "View log" button back to the log-only surface plus the same
        // `...` menu (exports + delete). Resolved by index like the
        // viewer does; a row deleted underneath the player simply
        // drops the affordances.
        if let Some(idx) = self.session_logs.iter().position(|e| e.id == p.log_id) {
            header_items.push(super::history::viewer_header_btn(
                iced_fonts::lucide::file_text()
                    .size(11)
                    .color(OryxisColors::t().text_secondary)
                    .into(),
                Some(crate::i18n::t("player_view_log")),
                Message::History(HistoryMessage::ViewSessionLog(p.log_id)),
            ));
            header_items.push(Space::new().width(8).into());
            let menu_open = matches!(
                self.overlay.as_ref().map(|o| &o.content),
                Some(crate::state::OverlayContent::SessionLogViewerActions(i)) if *i == idx
            );
            let kebab = super::history::viewer_header_btn(
                // Same glyph the card kebabs draw.
                text("\u{22EE}")
                    .size(13)
                    .color(if menu_open {
                        OryxisColors::t().text_primary
                    } else {
                        OryxisColors::t().text_muted
                    })
                    .into(),
                None,
                Message::History(HistoryMessage::ShowSessionLogViewerMenu(idx)),
            );
            // No tooltip while the menu is up: the cursor is still over
            // the button right after the opening click, and the tooltip
            // is an iced overlay, so it would paint on top of the menu
            // anchored below.
            header_items.push(if menu_open {
                kebab
            } else {
                crate::views::terminal::icon_tooltip(kebab, crate::i18n::t("more_actions"))
            });
            header_items.push(Space::new().width(8).into());
        }
        header_items.push(
            button(
                container(
                    text(crate::i18n::t("close")).size(11).font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::new(crate::theme::SYSTEM_UI_FAMILY)
                    }).color(OryxisColors::t().text_muted),
                )
                .center_y(Length::Fixed(24.0))
                .padding(Padding { top: 0.0, right: 14.0, bottom: 0.0, left: 14.0 }),
            )
            .on_press(Message::Player(PlayerMessage::Close))
            .style(|_, status| {
                let bg = match status {
                    BtnStatus::Hovered => Color { a: 0.15, ..OryxisColors::t().error },
                    _ => Color::TRANSPARENT,
                };
                button::Style {
                    background: Some(Background::Color(bg)),
                    border: Border {
                        radius: Radius::from(6.0),
                        color: OryxisColors::t().border,
                        width: 1.0,
                    },
                    ..Default::default()
                }
            })
            .into(),
        );
        let header = container(
            crate::widgets::dir_row(header_items).align_y(iced::Alignment::Center),
        )
        .padding(Padding { top: 16.0, right: 20.0, bottom: 12.0, left: 20.0 });

        // ── Terminal canvas, pinned to the recording's geometry ──
        // The grid is fixed (the recorded resize events drive it), so
        // the canvas gets exactly the pixel size that grid needs. The
        // replay font shrinks below the configured size whenever that
        // grid would overflow the stage (issue #89: the player surface
        // loses height to the header and transport bar, so a recording
        // made on a bigger terminal always overflowed), bottoming out
        // at a legibility floor where the scrollbars take over.
        // Display-only: the emulation keeps the recorded geometry, so
        // full-screen apps (vim, tmux) replay exactly as captured.
        let term_bg = {
            // The palette was resolved at open; read it off the state
            // like the link chip does (non-blocking; a missed frame
            // just paints the previous background).
            p.terminal
                .try_lock()
                .map(|s| s.palette.background)
                .unwrap_or(OryxisColors::t().bg_primary)
        };
        let stage = iced::widget::responsive(move |avail| {
            // The font is fitted against the recording's LARGEST frame
            // (`fit_cols`/`fit_rows`), not the current one, so it stays
            // constant across a session that was resized mid-recording
            // instead of jumping at every resize event. Every actual
            // frame is <= the fit geometry, so none overflow; the canvas
            // below is still sized to the CURRENT grid, so a smaller
            // frame just centers with margins at the same font.
            let font_size = fitted_font_size(
                &self.terminal_font_name,
                self.terminal_font_weight.font_weight(),
                self.terminal_font_size,
                p.fit_cols,
                p.fit_rows,
                iced::Size::new(
                    (avail.width - STAGE_PAD * 2.0).max(0.0),
                    (avail.height - STAGE_PAD * 2.0).max(0.0),
                ),
            );
            let (px_w, px_h) = oryxis_terminal::widget::grid_pixel_size(
                &self.terminal_font_name,
                self.terminal_font_weight.font_weight(),
                font_size,
                p.cols,
                p.rows,
            );
            let term_view = TerminalView::new(Arc::clone(&p.terminal))
                .with_fixed_grid(true)
                // No mouse-tracking reports ever leave a replay (there
                // is nothing to receive them); selection/copy stay
                // local.
                //
                // Unfocused on purpose, and not for the reports (those
                // are off below): the keys over this surface are the
                // transport (Space, seek, speed), so the widget's
                // "typing clears the highlight" rule must not apply, or
                // seeking after a selection would drop it. The chords
                // still fire, through `with_chords_unfocused`: nothing
                // else on screen is a terminal while the player is up
                // (it is mutually exclusive with the transcript viewer,
                // and the History view has no live pane), so the focus
                // gate has no ambiguity left to resolve here.
                .focused(false)
                .with_terminal_chords(replay_chords(self.terminal_chord_resolver()))
                .with_chords_unfocused(true)
                .with_mouse_reporting(false)
                .with_font_size(font_size)
                .with_font_name(&self.terminal_font_name)
                .with_font_weight(self.terminal_font_weight.font_weight())
                .with_text_dilation(self.terminal_text_thickness.px())
                .with_copy_on_select(self.prefs.copy_on_select)
                .with_right_click_copy(self.prefs.right_click_copy)
                // Same right-click scheme as a live pane. Without it the
                // replay fell back to the widget default, so a user who
                // had picked "paste" or "extend selection" got a
                // different gesture here than everywhere else.
                .with_right_click_action(self.prefs.terminal_right_click.to_widget())
                .with_bold_is_bright(self.prefs.bold_is_bright)
                .with_keyword_highlight(self.prefs.keyword_highlight)
                // Same as the history viewer: the replay is painted by
                // the rules, never watched by them.
                .with_highlight_rules(self.prefs.compiled_highlight_rules.clone())
                .with_performance(self.prefs.performance_mode)
                .with_privacy(mask)
                .with_privacy_terms(&self.privacy_terms())
                .with_privacy_classes(self.privacy_classes())
                .with_smart_contrast(self.prefs.smart_contrast)
                .with_word_delimiters(&self.prefs.word_delimiters)
                // A link in a replay is remote text like a link in a live
                // pane, so it takes the same confirmation on the way out
                // instead of the widget's own opener.
                .on_link_activate(|url| {
                    Message::Terminal(crate::app::TerminalMessage::TerminalLinkActivatedInRecording(url))
                });
            let term_canvas = canvas(term_view)
                .width(Length::Fixed(px_w))
                .height(Length::Fixed(px_h));
            // Inside a both-axis scrollable `Length::Fill` collapses to
            // the content size, so center by sizing the container to
            // the larger of the stage and the (padded) canvas: a
            // fitted frame centers on both axes, an overflowing one
            // (fit at the floor) keeps its natural size and scrolls.
            Element::from(
                scrollable(
                    container(term_canvas)
                        .padding(STAGE_PAD)
                        .width(Length::Fixed(
                            avail.width.max(px_w + STAGE_PAD * 2.0),
                        ))
                        .height(Length::Fixed(
                            avail.height.max(px_h + STAGE_PAD * 2.0),
                        ))
                        .align_x(iced::alignment::Horizontal::Center)
                        .align_y(iced::alignment::Vertical::Center),
                )
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .height(Length::Fill),
            )
        });
        let stage = container(stage)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(term_bg)),
                ..Default::default()
            });

        // ── Transport bar ──
        let (play_icon, play_tip) = if p.playing {
            (iced_fonts::lucide::pause(), crate::i18n::t("player_pause_tip"))
        } else {
            (iced_fonts::lucide::play(), crate::i18n::t("player_play_tip"))
        };
        let play_btn = transport_btn(play_icon, play_tip, Message::Player(PlayerMessage::TogglePlay));
        let restart_btn = transport_btn(
            iced_fonts::lucide::rotate_ccw(),
            crate::i18n::t("player_restart_tip"),
            Message::Player(PlayerMessage::Restart),
        );
        // The knob and label follow the live scrub target while dragging,
        // the playback clock otherwise.
        let display_ms = p.display_ms();
        let time_label = format!(
            "{} / {}",
            format_clock(display_ms as i64),
            format_clock(p.duration_ms),
        );
        // Scrubber over the full timeline in milliseconds. Dragging only
        // records the target (cheap); the one rebuild/replay a backward
        // jump needs happens on release, not per per-ms slider event.
        let scrubber = iced::widget::slider(
            0.0..=(p.duration_ms.max(1) as f64),
            display_ms.clamp(0.0, p.duration_ms as f64),
            |v| Message::Player(PlayerMessage::Scrub(v)),
        )
        .on_release(Message::Player(PlayerMessage::ScrubCommit))
        .step(1.0)
        .width(Length::Fill);
        // Speed chip: cycles the preset steps; trailing x reads the
        // same in every locale.
        let speed_label = if (p.speed.fract()).abs() < f32::EPSILON {
            format!("{}x", p.speed as u32)
        } else {
            format!("{}x", p.speed)
        };
        let speed_btn = crate::views::terminal::icon_tooltip(
            button(
                container(
                    text(speed_label)
                        .size(11)
                        .font(iced::Font {
                            weight: iced::font::Weight::Bold,
                            ..iced::Font::new(crate::theme::SYSTEM_UI_FAMILY)
                        })
                        .color(OryxisColors::t().text_secondary),
                )
                .center_y(Length::Fixed(24.0))
                .center_x(Length::Fixed(40.0)),
            )
            .on_press(Message::Player(PlayerMessage::SpeedCycle))
            .style(|_, status| transport_style(status))
            .into(),
            crate::i18n::t("player_speed_tip"),
        );
        let controls = container(
            crate::widgets::dir_row(vec![
                play_btn,
                Space::new().width(4).into(),
                restart_btn,
                Space::new().width(12).into(),
                text(time_label)
                    .size(11)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                Space::new().width(12).into(),
                scrubber.into(),
                Space::new().width(12).into(),
                speed_btn,
            ])
            .align_y(iced::Alignment::Center),
        )
        .padding(Padding { top: 10.0, right: 20.0, bottom: 12.0, left: 20.0 })
        .width(Length::Fill);

        container(column![header, stage, controls])
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(OryxisColors::t().bg_primary)),
                ..Default::default()
            })
            .into()
    }
}

/// Padding around the replay canvas inside the stage.
const STAGE_PAD: f32 = 16.0;

/// Legibility floor for the fitted replay font: below this the fit
/// gives up and the stage's scrollbars take over.
const MIN_REPLAY_FONT: f32 = 6.0;

/// Fit walk step. Half-pixel granularity keeps the per-(family, size)
/// glyph-metric cache small and the fitted size stable across tiny
/// stage jitters.
const FIT_STEP: f32 = 0.5;

/// Largest font size, capped at the configured `base`, at which a
/// pinned `cols` x `rows` grid fits inside `avail` (issue #89). Walks
/// down from `base` in [`FIT_STEP`] steps; every probed size lands in
/// the shared `cell_advance` cache, so steady-state layout passes cost
/// one lookup per step. Bottoms out at [`MIN_REPLAY_FONT`].
fn fitted_font_size(
    font_name: &str,
    font_weight: iced::font::Weight,
    base: f32,
    cols: u16,
    rows: u16,
    avail: iced::Size,
) -> f32 {
    let fits = |size: f32| {
        let (w, h) = oryxis_terminal::widget::grid_pixel_size(
            font_name,
            font_weight,
            size,
            cols,
            rows,
        );
        w <= avail.width && h <= avail.height
    };
    let mut size = base.max(MIN_REPLAY_FONT);
    while size > MIN_REPLAY_FONT && !fits(size) {
        size = (size - FIT_STEP).max(MIN_REPLAY_FONT);
    }
    size
}

/// Shared style for the transport-bar icon buttons: transparent at
/// rest, `bg_hover` fill on hover, accent tint on press (the app-wide
/// button-feedback convention).
fn transport_style(status: BtnStatus) -> button::Style {
    let bg = match status {
        BtnStatus::Hovered => OryxisColors::t().bg_hover,
        BtnStatus::Pressed => Color { a: 0.25, ..OryxisColors::t().accent },
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: Radius::from(6.0),
            color: OryxisColors::t().border,
            width: 1.0,
        },
        ..Default::default()
    }
}

/// One transport icon button with its tooltip.
fn transport_btn<'a>(
    icon: iced::widget::Text<'a>,
    tip: &'a str,
    msg: Message,
) -> Element<'a, Message> {
    crate::views::terminal::icon_tooltip(
        button(
            container(icon.size(14).color(OryxisColors::t().text_secondary))
                .center(Length::Fixed(28.0)),
        )
        .on_press(msg)
        .style(|_, status| transport_style(status))
        .into(),
        tip,
    )
}

/// `m:ss` (or `h:mm:ss` past the hour) for the transport clock.
fn format_clock(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::{fitted_font_size, format_clock, MIN_REPLAY_FONT};
    use iced::font::Weight;

    // Without a live renderer `cell_advance` uses its deterministic
    // `font_size * 0.6` fallback, so grid sizes here are exact:
    // width = cols * size * 0.6 + 16, height = rows * size * 1.15 + 16.

    #[test]
    fn fit_keeps_the_configured_size_when_the_grid_fits() {
        // 80x24 at 14 px ≈ 688x403: comfortably inside 1000x600.
        let size = fitted_font_size(
            "Test Mono",
            Weight::Normal,
            14.0,
            80,
            24,
            iced::Size::new(1000.0, 600.0),
        );
        assert_eq!(size, 14.0);
    }

    #[test]
    fn fit_shrinks_an_oversized_grid_to_the_stage() {
        // 200x50 at 14 px ≈ 1696x821: must shrink for an 800x500 stage.
        let avail = iced::Size::new(800.0, 500.0);
        let size = fitted_font_size("Test Mono", Weight::Normal, 14.0, 200, 50, avail);
        assert!(size < 14.0, "oversized grid must shrink, got {size}");
        assert!(size >= MIN_REPLAY_FONT);
        let (w, h) = oryxis_terminal::widget::grid_pixel_size(
            "Test Mono",
            Weight::Normal,
            size,
            200,
            50,
        );
        assert!(w <= avail.width && h <= avail.height, "fitted grid overflows: {w}x{h}");
    }

    #[test]
    fn fit_bottoms_out_at_the_floor_for_degenerate_stages() {
        // Nothing readable fits a 100x80 stage; the floor wins and the
        // scrollbars take over.
        let size = fitted_font_size(
            "Test Mono",
            Weight::Normal,
            14.0,
            200,
            50,
            iced::Size::new(100.0, 80.0),
        );
        assert_eq!(size, MIN_REPLAY_FONT);
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(format_clock(0), "0:00");
        assert_eq!(format_clock(59_999), "0:59");
        assert_eq!(format_clock(65_000), "1:05");
        assert_eq!(format_clock(3_600_000), "1:00:00");
        assert_eq!(format_clock(-5), "0:00");
    }
}

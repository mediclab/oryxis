//! History view: unified timeline of connection errors + recorded
//! sessions, replacing the separate "History" / "Session Logs"
//! containers from v0.6. Successful connect/disconnect events are
//! folded into the corresponding session log row (start/end times,
//! data size) so the list reads as one chronological feed.

use std::sync::Arc;

use iced::border::Radius;
use iced::widget::{button, canvas, column, container, scrollable, text, Space};
use iced::widget::button::Status as BtnStatus;
use iced::{Background, Border, Color, Element, Length, Padding};

use oryxis_terminal::widget::TerminalView;

use chrono::{DateTime, Utc};

use crate::app::{HistoryMessage, PlayerMessage, Message, Oryxis};
use crate::theme::OryxisColors;
use crate::util::format_data_size;

/// A single row in the unified timeline, either a failed-connect log
/// entry or a recorded session. Ordered by `ts` descending across
/// both kinds.
enum TimelineKind<'a> {
    /// Connection attempt that didn't go anywhere useful (auth
    /// failure or transport error). Carries the original LogEntry
    /// so we can show the underlying message.
    Failure(&'a oryxis_core::models::log_entry::LogEntry),
    /// A recorded session, successful start through end (or still
    /// in progress). Carries the session row + its index in
    /// `self.session_logs` so the View/Delete buttons can target it.
    Session {
        idx: usize,
        entry: &'a oryxis_vault::SessionLogEntry,
    },
    /// A saved AI conversation. Shares the timeline with the sessions
    /// rather than living in its own tab, so a chat sits next to the
    /// session it was held during, which is how anyone looking for it
    /// remembers it.
    Chat {
        idx: usize,
        entry: &'a oryxis_vault::ChatConversationEntry,
    },
}

struct TimelineRow<'a> {
    ts: DateTime<Utc>,
    label: &'a str,
    hostname: Option<&'a str>,
    kind: TimelineKind<'a>,
}

impl Oryxis {
    pub(crate) fn view_history(&self) -> Element<'_, Message> {
        use oryxis_core::models::log_entry::LogEvent;

        // Fully empty: no recorded sessions and no failed-connect logs
        // that would become timeline rows. (Plain Connected/Disconnected
        // events live in `self.logs` but never render as rows, so checking
        // `self.logs.is_empty()` would leave the toolbar up with "0
        // entries".) Show just the empty state, no toolbar / pagination /
        // Clear all, matching the other empty vault views.
        let has_timeline = !self.session_logs.is_empty()
            || !self.chat_ui.conversations.is_empty()
            || self
                .logs
                .iter()
                .any(|e| matches!(e.event, LogEvent::AuthFailed | LogEvent::Error));
        if !has_timeline {
            // No toolbar / rows on this path; drop anything recorded
            // by a previous frame so the keyboard router matches.
            self.keynav_toolbar_reset();
            self.keynav_clear_content();
            return column![crate::widgets::empty_state(
                iced_fonts::lucide::history()
                    .size(32)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                crate::i18n::t("no_activity").to_string(),
                crate::i18n::t("no_activity_desc").to_string(),
                None,
            )]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }

        // ── Toolbar ──
        let per_page: usize = 50;
        let needle = self.history_search.trim().to_lowercase();
        // Content-search results only apply when they answer the live
        // query: typing is debounced and the output scan is async, so
        // the maps can lag a keystroke behind; a stale needle falls
        // back to the plain label/hostname filter until they catch up.
        let content_ready = self.history_search_content
            && !needle.is_empty()
            && self.history_content.needle == needle;

        // Build the unified timeline. Failed log entries (auth fail,
        // transport error) stay as their own rows; everything else
        // about a successful connect is already captured by its
        // session log row, so we drop Connected/Disconnected events
        // here to avoid showing the same connection twice.
        let mut rows: Vec<TimelineRow<'_>> = Vec::new();
        for entry in &self.logs {
            if !matches!(entry.event, LogEvent::AuthFailed | LogEvent::Error) {
                continue;
            }
            rows.push(TimelineRow {
                ts: entry.timestamp,
                label: &entry.connection_label,
                hostname: Some(&entry.hostname),
                kind: TimelineKind::Failure(entry),
            });
        }
        // One-pass lookup maps so each row resolves its connection in
        // O(1) instead of scanning the full connection list per row on
        // every frame. `conn_by_label` keeps the first match to mirror
        // the old `find` semantics for duplicate labels.
        let conn_by_id: std::collections::HashMap<
            uuid::Uuid,
            &oryxis_core::models::connection::Connection,
        > = self.connections.iter().map(|c| (c.id, c)).collect();
        let mut conn_by_label: std::collections::HashMap<&str, _> =
            std::collections::HashMap::new();
        for c in &self.connections {
            conn_by_label.entry(c.label.as_str()).or_insert(c);
        }
        for (idx, entry) in self.session_logs.iter().enumerate() {
            // Look up the connection by id so we can show its
            // hostname next to the label (matches the Termius row).
            let hostname = conn_by_id
                .get(&entry.connection_id)
                .map(|c| c.hostname.as_str());
            rows.push(TimelineRow {
                ts: entry.started_at,
                label: &entry.label,
                hostname,
                kind: TimelineKind::Session { idx, entry },
            });
        }
        for (idx, entry) in self.chat_ui.conversations.iter().enumerate() {
            // A conversation on a local shell has no host to resolve.
            let hostname = entry
                .connection_id
                .and_then(|id| conn_by_id.get(&id))
                .map(|c| c.hostname.as_str());
            rows.push(TimelineRow {
                // Placed by when the conversation STARTED, like a session,
                // so the timeline reads as "what happened when".
                ts: entry.started_at,
                label: &entry.label,
                hostname,
                kind: TimelineKind::Chat { idx, entry },
            });
        }

        // Host-tag filter first: rows resolve to their connection
        // (session rows by id, failure rows by label) and survive when
        // it carries any selected tag; a deleted host can't match.
        // Composes with the search below as an AND.
        if !self.history_filter_tags.is_empty() {
            rows.retain(|r| {
                let conn = match &r.kind {
                    TimelineKind::Session { entry, .. } => {
                        conn_by_id.get(&entry.connection_id).copied()
                    }
                    TimelineKind::Chat { entry, .. } => entry
                        .connection_id
                        .and_then(|id| conn_by_id.get(&id).copied()),
                    TimelineKind::Failure(_) => conn_by_label.get(r.label).copied(),
                };
                conn.is_some_and(|c| {
                    c.tags.iter().any(|tg| {
                        self.history_filter_tags
                            .iter()
                            .any(|f| f.eq_ignore_ascii_case(tg))
                    })
                })
            });
        }

        // Filter by the contextual sub-nav search before paginating
        // so the page counts reflect what the user actually sees.
        // Filtering before the sort also keeps the sort to the rows
        // that survive. With the content toggle on, a session row also
        // survives when the async search matched one of its recorded
        // commands or its output.
        if !needle.is_empty() {
            rows.retain(|r| {
                if r.label.to_lowercase().contains(&needle)
                    || r.hostname.is_some_and(|h| h.to_lowercase().contains(&needle))
                {
                    return true;
                }
                content_ready
                    && match &r.kind {
                        TimelineKind::Session { entry, .. } => {
                            self.history_content.log_matches.contains_key(&entry.id)
                        }
                        // The content search scans recorded terminal
                        // output; a conversation has none, so it only ever
                        // matches on the label/host search above.
                        TimelineKind::Chat { .. } | TimelineKind::Failure(_) => false,
                    }
            });
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.ts));

        let total = rows.len();
        let max_page = total.saturating_sub(1) / per_page.max(1);
        let page = self.logs_page.min(max_page);
        let can_prev = page > 0;
        let can_next = page < max_page;
        let range_label = if total == 0 {
            format!("0 {}", crate::i18n::t("entries"))
        } else {
            let start = page * per_page + 1;
            let end = ((page + 1) * per_page).min(total);
            format!(
                "{}\u{2013}{} {} {}",
                start, end, crate::i18n::t("of"), total
            )
        };

        let prev_btn = nav_btn(
            iced_fonts::lucide::chevron_left(),
            Message::History(HistoryMessage::LogsPagePrev),
            can_prev,
        );
        let next_btn = nav_btn(
            iced_fonts::lucide::chevron_right(),
            Message::History(HistoryMessage::LogsPageNext),
            can_next,
        );

        // "Clear all" reads as a destructive main action (solid error
        // fill, same look as the confirm modal's primary button) and
        // only *requests* the wipe; the actual ClearLogs runs from the
        // confirmation modal in layout.rs.
        // Nothing to clear → the button is disabled (muted, no action).
        let has_entries = !self.logs.is_empty()
            || !self.session_logs.is_empty()
            || !self.chat_ui.conversations.is_empty();
        let mut clear_btn = button(
            container(
                text(crate::i18n::t("clear_all").to_uppercase())
                    .size(11)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::new(crate::theme::SYSTEM_UI_FAMILY)
                    })
                    .color(OryxisColors::t().button_text),
            )
            .center_y(Length::Fixed(24.0))
            .padding(Padding {
                top: 0.0,
                right: 14.0,
                bottom: 0.0,
                left: 14.0,
            }),
        )
        .style(|_, status| {
            let base = OryxisColors::t().error;
            let bg = match status {
                BtnStatus::Hovered => Color { a: 0.85, ..base },
                BtnStatus::Pressed => Color { a: 0.70, ..base },
                // Disabled (no entries): muted so it reads as inactive.
                BtnStatus::Disabled => Color { a: 0.30, ..base },
                _ => base,
            };
            button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    radius: Radius::from(6.0),
                    ..Default::default()
                },
                ..Default::default()
            }
        });
        if has_entries {
            clear_btn = clear_btn.on_press(Message::History(HistoryMessage::RequestClearHistory));
        }

        // Responsive collapse: search yields first, then folds to an icon;
        // when the cluster (range label + prev/next pager + "Clear all")
        // can't fit, it all moves into the `…` overflow menu.
        let (search_collapsed, buttons_overflow) = self.toolbar_tiers();
        let overflow_open = matches!(
            self.overlay.as_ref().map(|o| &o.content),
            Some(crate::state::OverlayContent::ToolbarOverflow)
        );
        // `keynav_toolbar_slot` records each rendered action for the
        // keyboard router (push order == visual order here). Disabled
        // controls (dead pager edge, empty "Clear all") are skipped so
        // the keyboard never lands on a button that does nothing.
        self.keynav_toolbar_reset();
        let search_slot = self.vault_search_slot(search_collapsed);
        let search_slot = if search_collapsed {
            self.keynav_toolbar_slot(crate::keynav::ToolbarItem::SearchIcon, search_slot)
        } else {
            search_slot
        };
        // The content-toggle chip lives INSIDE the search field
        // (vault_search_field applies its ring); record it whenever
        // that field is on screen, inline or in the collapsed-search
        // overlay, so the toolbar walk reaches it.
        let search_overlay_open = matches!(
            self.overlay.as_ref().map(|o| &o.content),
            Some(crate::state::OverlayContent::ToolbarSearch)
        );
        if !search_collapsed || search_overlay_open {
            self.keynav_toolbar_record(crate::keynav::ToolbarItem::SearchContent);
        }
        let mut row_items: Vec<Element<'_, Message>> =
            vec![search_slot, Space::new().width(10).into()];
        // Output-scan progress: the command tiers answer instantly,
        // the recorded-output pass streams in; say where it stands so
        // a still-filling result list reads as "working", not "done".
        if content_ready
            && (self.history_content.scanning || !self.history_content.queue.is_empty())
        {
            row_items.push(
                text(
                    crate::i18n::t("history_scanning")
                        .replace("{done}", &self.history_content.scan_done.to_string())
                        .replace("{total}", &self.history_content.scan_total.to_string()),
                )
                .size(11)
                .color(OryxisColors::t().text_muted)
                .into(),
            );
            row_items.push(Space::new().width(8).into());
        }
        // Host-tag filter, mirroring the dashboard's: only rendered
        // once at least one host is tagged (or a filter needs
        // clearing); folds into the `…` menu with the other buttons.
        if !buttons_overflow && self.history_tag_filter_available() {
            row_items.push(self.keynav_toolbar_slot(
                crate::keynav::ToolbarItem::TagFilter,
                crate::widgets::bounds_reporter(
                    crate::widgets::tag_filter_toolbar_button(
                        self.history_filter_tags.len(),
                        Message::History(HistoryMessage::ShowHistoryTagFilterMenu),
                    ),
                    self.history_tag_filter_btn_bounds.clone(),
                ),
            ));
            row_items.push(Space::new().width(8).into());
        }
        // Privacy reveal toggle, shown whenever Privacy Mode could mask
        // something in this view (global on, or any host forces it on).
        let privacy_applies = self.privacy_global_active()
            || self.connections.iter().any(|c| c.privacy_mode == Some(true));
        if privacy_applies {
            row_items.push(self.keynav_toolbar_slot(
                crate::keynav::ToolbarItem::PrivacyReveal,
                crate::widgets::privacy_reveal_btn(self.privacy.revealed),
            ));
            row_items.push(Space::new().width(8).into());
        }
        if buttons_overflow {
            row_items.push(self.keynav_toolbar_slot(
                crate::keynav::ToolbarItem::Overflow,
                crate::widgets::bounds_reporter(
                    crate::widgets::toolbar_overflow_icon(overflow_open),
                    self.toolbar_overflow_btn_bounds.clone(),
                ),
            ));
        } else {
            let prev_slot = if can_prev {
                self.keynav_toolbar_slot(crate::keynav::ToolbarItem::PagerPrev, prev_btn)
            } else {
                prev_btn
            };
            let next_slot = if can_next {
                self.keynav_toolbar_slot(crate::keynav::ToolbarItem::PagerNext, next_btn)
            } else {
                next_btn
            };
            let clear_slot: Element<'_, Message> = if has_entries {
                self.keynav_toolbar_slot(crate::keynav::ToolbarItem::Primary, clear_btn.into())
            } else {
                clear_btn.into()
            };
            row_items.extend([
                text(range_label)
                    .size(11)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                Space::new().width(8).into(),
                prev_slot,
                Space::new().width(4).into(),
                next_slot,
                Space::new().width(12).into(),
                clear_slot,
            ]);
        }
        let toolbar = container(
            crate::widgets::dir_row(row_items).align_y(iced::Alignment::Center),
        )
        .padding(Padding { top: 16.0, right: 24.0, bottom: 16.0, left: 24.0 })
        .width(Length::Fill);

        // ── Rows ──
        let list: Element<'_, Message> = if rows.is_empty() {
            // Nothing navigable (e.g. search with no match); keep the
            // keyboard order in sync with the screen.
            self.keynav_clear_content();
            crate::widgets::empty_state(
                iced_fonts::lucide::history()
                    .size(32)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                crate::i18n::t("no_activity").to_string(),
                crate::i18n::t("no_activity_desc").to_string(),
                None,
            )
        } else {
            let mut row_elements: Vec<Element<'_, Message>> = Vec::new();
            // Keyboard-navigation order for the current page: one row
            // per session entry. Failure rows have no click action, so
            // the keyboard skips them too (nothing to activate).
            let mut nav_rows: Vec<Vec<crate::keynav::NavItem>> = Vec::new();
            let start = page * per_page;
            let end = ((page + 1) * per_page).min(total);
            for row_data in &rows[start..end] {
                if let TimelineKind::Session { entry, .. } = &row_data.kind {
                    nav_rows.push(vec![crate::keynav::NavItem::HistoryLog(entry.id)]);
                }
                // Resolve the row's connection by ID whenever the row
                // carries one (session rows always do): the label is a
                // display string, so a renamed host would miss a
                // label lookup and everything derived from `conn`,
                // including the Privacy Mode mask over the decrypted
                // content excerpt below, would silently fall back to
                // the global default. Failure rows never recorded a
                // connection id, so the label lookup remains their
                // best effort (mirrors the tag filter above).
                let conn = match &row_data.kind {
                    TimelineKind::Session { entry, .. } => {
                        conn_by_id.get(&entry.connection_id).copied()
                    }
                    TimelineKind::Chat { entry, .. } => entry
                        .connection_id
                        .and_then(|id| conn_by_id.get(&id).copied()),
                    TimelineKind::Failure(_) => conn_by_label.get(row_data.label).copied(),
                };
                // The content search knows WHY a session matched (the
                // command line, or an output excerpt); surface it on
                // the row so the hit is self-explanatory.
                let content_hit = if content_ready {
                    match &row_data.kind {
                        TimelineKind::Session { entry, .. } => self
                            .history_content
                            .log_matches
                            .get(&entry.id)
                            .map(|s| s.as_str()),
                        TimelineKind::Chat { .. } | TimelineKind::Failure(_) => None,
                    }
                } else {
                    None
                };
                row_elements.push(self.render_timeline_row(row_data, conn, content_hit));
                row_elements.push(Space::new().height(4).into());
            }
            self.keynav_set_content_rows(nav_rows);
            scrollable(
                column(row_elements).padding(Padding {
                    top: 0.0,
                    right: 24.0,
                    bottom: 24.0,
                    left: 24.0,
                }),
            )
            // Stable id so the keyboard router can keep the selected
            // row scrolled into view.
            .id(iced::widget::Id::new("history-list-scroll"))
            .height(Length::Fill)
            .into()
        };

        // ── Session player surface (issue #71) ──
        // Takes the whole history area while open, like the static
        // viewer below; mutually exclusive with it by construction
        // (opening either closes the other in the dispatcher).
        if let Some(player) = &self.session_player {
            return self.view_session_player(player);
        }

        // ── Saved conversation reader ──
        // Same slot as the recording viewer below: opening one closes the
        // other in the dispatcher, so only one can be up.
        if let Some(viewer) = &self.chat_ui.viewer {
            return self.view_chat_viewer(viewer);
        }

        // ── Session viewer overlay ──
        if let Some(viewer) = &self.viewing_session_log {
            let log_id = viewer.log_id;
            // The transcript background wears the theme's terminal
            // background; the recording's own colors live inside the
            // emulator the widget renders.
            let term_bg = self.resolve_global_terminal_palette().background;
            // Resolve the recording's host to decide Privacy Mode. Per-host
            // override wins; a deleted host falls back to the global default.
            let viewer_conn = self
                .session_logs
                .iter()
                .find(|e| e.id == log_id)
                .and_then(|e| self.connections.iter().find(|c| c.id == e.connection_id));
            let privacy_applies = viewer_conn
                .map(|c| self.privacy_active(c))
                .unwrap_or_else(|| self.privacy_global_active());
            let mask = privacy_applies && !self.privacy.revealed;
            // The transcript renders through the real terminal widget
            // (issues #90/#91): read-only, PTY-less, so selection,
            // copy-on-select and the right-click schemes match the live
            // pane and the selection highlight is cell-exact. No input
            // callbacks are wired; the widget's own scrollback scrolls it,
            // so it is NOT nested in an iced scrollable.
            let term_view = TerminalView::new(Arc::clone(&viewer.terminal))
                // Focused so the widget's keyboard chords fire: Ctrl+Shift+C
                // (the copy path that worked in the old viewer), Select All,
                // and PageUp/Down over the recording's scrollback. Safe
                // because the History and Terminal views never render at the
                // same time, so no live pane competes for focus. Plain keys
                // are not captured here (they fall through), so the toolbar
                // search still receives its input.
                .focused(true)
                .with_mouse_reporting(false)
                .with_font_size(self.terminal_font_size)
                .with_font_name(&self.terminal_font_name)
                .with_font_weight(self.terminal_font_weight.font_weight())
                .with_text_dilation(self.terminal_text_thickness.px())
                .with_copy_on_select(self.prefs.copy_on_select)
                .with_right_click_copy(self.prefs.right_click_copy)
                .with_right_click_action(self.prefs.terminal_right_click.to_widget())
                .with_terminal_chords(self.terminal_chord_resolver())
                // A recording has no live edge to snap back to, and the
                // "reset on output" snap must not fight the open-at-top
                // scroll (the whole recording was fed before the first draw,
                // so its epoch bump would otherwise pull the view to the
                // bottom). The keypress half needs no opt-out here: the
                // player never writes to a PTY, which is where the app
                // queues that snap (issue #111).
                .with_reset_scroll_on_output(false)
                .with_bold_is_bright(self.prefs.bold_is_bright)
                .with_keyword_highlight(self.prefs.keyword_highlight)
                // Colours only: this backend is never given the rules, so
                // a recording can be read without its triggers firing
                // again years later.
                .with_highlight_rules(self.prefs.compiled_highlight_rules.clone())
                .with_performance(self.prefs.performance_mode)
                .with_privacy(mask)
                .with_privacy_terms(&self.privacy_terms())
                .with_privacy_classes(self.privacy_classes())
                .with_smart_contrast(self.prefs.smart_contrast)
                .with_word_delimiters(&self.prefs.word_delimiters)
                // Same confirmation as a live remote pane: the transcript
                // is the remote host's text, OSC 8 labels included.
                .on_link_activate(|url| {
                    Message::Terminal(crate::app::TerminalMessage::TerminalLinkActivatedInRecording(url))
                });
            // Right-click scheme = Menu: the widget has no menu of its
            // own, so wire the read-only transcript context menu (Copy /
            // Copy All). The Paste and Extend schemes copy a live
            // selection without a menu, so they need no wiring here.
            let term_view = if self.prefs.terminal_right_click
                == crate::util::RightClickMode::Menu
            {
                term_view.on_context_menu(|x, y, sel| {
                    Message::History(HistoryMessage::ShowSessionViewerContextMenu(x, y, sel))
                })
            } else {
                term_view
            };
            let body = container(
                canvas(term_view).width(Length::Fill).height(Length::Fill),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8)
            .style(move |_| container::Style {
                background: Some(Background::Color(term_bg)),
                ..Default::default()
            });
            // Header row: title, optional Privacy reveal toggle (only when
            // the recording's host is under Privacy Mode), then the
            // recording actions: Play (the primary verb, its own button,
            // owner call 2026-07-17), a `...` menu with the exports +
            // delete, and Close.
            let mut header_items: Vec<Element<'_, Message>> = vec![
                text(crate::i18n::t("session_log"))
                    .size(16)
                    .color(OryxisColors::t().text_primary)
                    .into(),
                Space::new().width(Length::Fill).into(),
            ];
            if privacy_applies {
                header_items.push(crate::widgets::privacy_reveal_btn(self.privacy.revealed));
                header_items.push(Space::new().width(8).into());
            }
            // Transcript mode switch. A recording that lived on the
            // alternate screen (a whole tmux session, a long pager) has
            // nothing to scroll in the faithful replay, so it opens in
            // Linear; anything else opens Rendered. Either way the reader
            // can flip, and the tooltip names the mode the click leads to.
            let next_mode = viewer.mode.toggled();
            header_items.push(crate::views::terminal::icon_tooltip(
                viewer_header_btn(
                    match next_mode {
                        crate::state::TranscriptMode::Linear => iced_fonts::lucide::list(),
                        crate::state::TranscriptMode::Rendered => iced_fonts::lucide::monitor(),
                    }
                    .size(11)
                    .color(OryxisColors::t().text_muted)
                    .into(),
                    None,
                    Message::History(HistoryMessage::ToggleSessionViewerMode),
                ),
                crate::i18n::t(next_mode.label_key()),
            ));
            header_items.push(Space::new().width(8).into());
            // Resolve the viewed recording's index for the actions menu;
            // a row deleted underneath the viewer resolves to None and
            // simply drops the affordances.
            let viewed_idx = self.session_logs.iter().position(|e| e.id == log_id);
            if let Some(idx) = viewed_idx {
                // Play pairs with full-detail recording, same gate as
                // the row menu (owner call 2026-07-04).
                if self.prefs.session_log_full {
                    header_items.push(viewer_header_btn(
                        iced_fonts::lucide::play()
                            .size(11)
                            .color(OryxisColors::t().success)
                            .into(),
                        Some(crate::i18n::t("session_play")),
                        Message::Player(PlayerMessage::Open(log_id)),
                    ));
                    header_items.push(Space::new().width(8).into());
                }
                let menu_open = matches!(
                    self.overlay.as_ref().map(|o| &o.content),
                    Some(crate::state::OverlayContent::SessionLogViewerActions(i)) if *i == idx
                );
                let kebab = viewer_header_btn(
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
                // No tooltip while the menu is up: the cursor is still
                // over the button right after the opening click, and the
                // tooltip is an iced overlay, so it would paint on top
                // of the menu anchored below.
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
                    .padding(Padding {
                        top: 0.0, right: 14.0, bottom: 0.0, left: 14.0,
                    }),
                )
                .on_press(Message::History(HistoryMessage::CloseSessionLogView))
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
            let viewer = container(
                column![
                    container(
                        crate::widgets::dir_row(header_items).align_y(iced::Alignment::Center),
                    )
                    .padding(Padding {
                        top: 16.0, right: 20.0, bottom: 12.0, left: 20.0,
                    }),
                    body,
                ],
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(OryxisColors::t().bg_primary)),
                border: Border {
                    radius: Radius::from(0.0),
                    ..Default::default()
                },
                ..Default::default()
            });

            return viewer.into();
        }

        // Search now lives in the toolbar (`vault_search_field`); the
        // legacy below-toolbar search bar collapses to nothing.
        let search_bar: Element<'_, Message> = Space::new().into();

        // ── Hosts with matching commands (content search) ──
        // A host whose command history matched but has no recorded
        // session leaves no timeline row to light up; this strip keeps
        // the "which hosts ran this command?" answer complete. Purely
        // informational (the rows below stay the interactive surface).
        let cmd_hosts: Element<'_, Message> = if content_ready
            && !self.history_content.conn_matches.is_empty()
        {
            const MAX_SHOWN: usize = 6;
            let mut lines: Vec<Element<'_, Message>> = Vec::new();
            let mut shown = 0usize;
            let mut hidden = 0usize;
            for (conn_id, cmd) in &self.history_content.conn_matches {
                // Deleted hosts have nothing to point at; the tag
                // filter narrows this strip like it narrows the rows.
                let Some(conn) = conn_by_id.get(conn_id).copied() else {
                    continue;
                };
                if !self.history_filter_tags.is_empty()
                    && !conn.tags.iter().any(|tg| {
                        self.history_filter_tags
                            .iter()
                            .any(|f| f.eq_ignore_ascii_case(tg))
                    })
                {
                    continue;
                }
                if shown >= MAX_SHOWN {
                    hidden += 1;
                    continue;
                }
                shown += 1;
                // Privacy Mode: labels mask like the timeline rows;
                // the command line is raw session content, redact it.
                let mask = !self.privacy.revealed && self.privacy_active(conn);
                let label = if mask {
                    crate::widgets::mask_blocks(&conn.label)
                } else {
                    conn.label.clone()
                };
                let cmd_disp = if mask {
                    crate::widgets::redact_for_display(
                        cmd,
                        &self.privacy_terms(),
                        self.privacy_classes(),
                    )
                } else {
                    cmd.clone()
                };
                lines.push(
                    crate::widgets::dir_row(vec![
                        text(label)
                            .size(11)
                            .color(OryxisColors::t().text_primary)
                            .into(),
                        Space::new().width(8).into(),
                        text(cmd_disp)
                            .size(11)
                            .color(OryxisColors::t().text_muted)
                            .into(),
                    ])
                    .align_y(iced::Alignment::Center)
                    .into(),
                );
                lines.push(Space::new().height(2).into());
            }
            if hidden > 0 {
                lines.push(
                    text(format!("+{hidden}"))
                        .size(11)
                        .color(OryxisColors::t().text_muted)
                        .into(),
                );
            }
            if lines.is_empty() {
                Space::new().into()
            } else {
                container(
                    container(
                        column![
                            crate::widgets::dir_row(vec![
                                iced_fonts::lucide::terminal()
                                    .size(12)
                                    .color(OryxisColors::t().accent)
                                    .into(),
                                Space::new().width(6).into(),
                                text(crate::i18n::t("history_cmd_hosts"))
                                    .size(11)
                                    .color(OryxisColors::t().text_muted)
                                    .into(),
                            ])
                            .align_y(iced::Alignment::Center),
                            Space::new().height(6),
                            column(lines).align_x(crate::widgets::dir_align_x()),
                        ]
                        .width(Length::Fill)
                        .align_x(crate::widgets::dir_align_x()),
                    )
                    .padding(Padding { top: 10.0, right: 16.0, bottom: 10.0, left: 16.0 })
                    .width(Length::Fill)
                    .style(|_| container::Style {
                        background: Some(Background::Color(OryxisColors::t().bg_surface)),
                        border: Border {
                            radius: Radius::from(8.0),
                            ..Default::default()
                        },
                        ..Default::default()
                    }),
                )
                .padding(Padding { top: 0.0, right: 24.0, bottom: 8.0, left: 24.0 })
                .width(Length::Fill)
                .into()
            }
        } else {
            Space::new().into()
        };

        column![toolbar, search_bar, cmd_hosts, list]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Render one row of the unified timeline. Layout (LTR):
    ///   [host_icon] [label / hostname] [event chip] [meta] [actions] [ts]
    /// `event chip` is "Session" / "Auth Failed" / "Error".
    /// `meta` and `actions` only show for session rows; failure rows
    /// add their underlying message under the label instead.
    fn render_timeline_row<'a>(
        &'a self,
        row: &TimelineRow<'a>,
        conn: Option<&'a oryxis_core::models::connection::Connection>,
        content_hit: Option<&'a str>,
    ) -> Element<'a, Message> {
        use oryxis_core::models::log_entry::LogEvent;

        // Host badge through the shared host_icon helper so per-host
        // shape + accent color are honoured here too. `conn` is the
        // row's connection, resolved by the caller through maps built
        // once per view call (by id for session rows, by label for
        // failure rows, which never carried an id); missing
        // connections (host deleted but log row stays) fall back to
        // the global accent.
        let icon_style = crate::widgets::resolve_host_icon_style(
            conn.and_then(|c| c.icon_style.as_deref()),
            &self.prefs.default_host_icon,
        );
        let detected_os = conn.and_then(|c| c.detected_os.as_deref());
        let (glyph, default_color) = crate::os_icon::resolve_icon(
            detected_os,
            OryxisColors::t().accent,
        );
        let icon_color = conn
            .and_then(|c| c.custom_color.as_deref().or(c.color.as_deref()))
            .and_then(crate::widgets::parse_hex_color)
            .unwrap_or(default_color);
        // Privacy Mode: mask the connection label (its own row title AND
        // the badge initials derived from it) plus the hostname below.
        // The label routinely carries user@host or the hostname itself,
        // so leaving it raw leaked exactly what the tab bar / status bar
        // / dashboard already mask. Per-host override wins; a deleted
        // host falls back to the global default; the Reveal toggle flips
        // it off.
        let mask = !self.privacy.revealed
            && conn
                .map(|c| self.privacy_active(c))
                .unwrap_or_else(|| self.privacy_global_active());
        let display_label: String = if mask {
            crate::widgets::mask_blocks(row.label)
        } else {
            row.label.to_string()
        };
        let glyph_el: Element<'_, Message> = glyph.view(14.0, Color::WHITE);
        let badge = crate::widgets::host_icon(
            icon_style,
            icon_color,
            &display_label,
            Some(glyph_el),
            28.0,
        );

        let ts = row
            .ts
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        // Event chip + per-kind colour. Session rows render in the
        // accent colour so they read as "primary" content vs. the
        // warning/error tint reserved for failures.
        let (chip_text, chip_color): (String, Color) = match &row.kind {
            TimelineKind::Failure(e) => match e.event {
                LogEvent::AuthFailed => (
                    crate::i18n::t("event_auth_failed").to_string(),
                    OryxisColors::t().warning,
                ),
                _ => (
                    crate::i18n::t("event_error").to_string(),
                    OryxisColors::t().error,
                ),
            },
            TimelineKind::Session { .. } => (
                crate::i18n::t("event_session").to_string(),
                OryxisColors::t().accent,
            ),
            TimelineKind::Chat { .. } => (
                crate::i18n::t("event_chat").to_string(),
                OryxisColors::t().success,
            ),
        };
        let chip = container(
            text(chip_text)
                .size(10)
                .color(chip_color),
        )
        .padding(Padding { top: 2.0, right: 8.0, bottom: 2.0, left: 8.0 })
        .style(move |_| container::Style {
            background: Some(Background::Color(Color { a: 0.12, ..chip_color })),
            border: Border {
                radius: Radius::from(4.0),
                ..Default::default()
            },
            ..Default::default()
        });

        // Privacy Mode (`mask` computed above with the label): mask the
        // hostname (a known sensitive value) and scrub any IP / user@host
        // inside a failure message.
        let host_str = match row.hostname {
            Some(h) if mask => crate::widgets::mask_blocks(h),
            Some(h) => h.to_string(),
            None => String::new(),
        };

        // Subtitle line: hostname for sessions, "hostname · message"
        // for failures (collapses to hostname when there's no message).
        let subtitle = match &row.kind {
            TimelineKind::Failure(e) => {
                let msg = if mask {
                    crate::widgets::redact_for_display(&e.message, &self.privacy_terms(), self.privacy_classes())
                } else {
                    e.message.clone()
                };
                if msg.is_empty() {
                    host_str.clone()
                } else if !host_str.is_empty() {
                    format!("{host_str} · {msg}")
                } else {
                    msg
                }
            }
            TimelineKind::Session { entry, .. } => {
                let hostname = host_str.as_str();
                let duration = if let Some(ended) = entry.ended_at {
                    let dur = ended.signed_duration_since(entry.started_at);
                    let secs = dur.num_seconds();
                    if secs < 60 {
                        format!("{}s", secs)
                    } else if secs < 3600 {
                        format!("{}m {}s", secs / 60, secs % 60)
                    } else {
                        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
                    }
                } else {
                    crate::i18n::t("in_progress").to_string()
                };
                let size_str = format_data_size(entry.data_size);
                if hostname.is_empty() {
                    format!("{} · {}", duration, size_str)
                } else {
                    format!("{hostname} · {duration} · {size_str}")
                }
            }
            // A conversation reads by its size and who answered, the
            // closest equivalent of a session's duration + byte count.
            TimelineKind::Chat { entry, .. } => {
                let turns = format!("{} {}", entry.message_count, crate::i18n::t("chat_turns"));
                let model = if entry.model.is_empty() {
                    entry.provider.clone()
                } else {
                    entry.model.clone()
                };
                if host_str.is_empty() {
                    format!("{turns} · {model}")
                } else {
                    format!("{host_str} · {turns} · {model}")
                }
            }
        };

        // Session rows are clickable (the whole row opens the
        // recording) and highlight on hover; failure rows have nothing
        // to open, so they keep the flat card look.
        let viewable = match &row.kind {
            TimelineKind::Session { entry, .. } => Some(entry.id),
            TimelineKind::Chat { entry, .. } => Some(entry.id),
            TimelineKind::Failure(_) => None,
        };
        let kb_selected = viewable.is_some()
            && self.keynav.selected_in(crate::keynav::FocusZone::Content)
                == viewable.map(crate::keynav::NavItem::HistoryLog);
        // Keyboard selection reuses the hover treatment (bg tint) plus
        // the shared ring below.
        let hovered = (viewable.is_some() && viewable == self.hover.log_row) || kb_selected;

        // Trailing controls. Session rows: timestamp, then the shared
        // kebab (Export .cast / Export transcript / Delete live in its
        // context menu, the app-wide card convention); opening the
        // recording is the row click itself. Failure rows: timestamp
        // only.
        let trailing: Element<'_, Message> = match &row.kind {
            TimelineKind::Session { idx, entry } => {
                let idx = *idx;
                let menu_open = matches!(
                    self.overlay.as_ref().map(|o| &o.content),
                    Some(crate::state::OverlayContent::SessionLogActions(i)) if *i == idx
                );
                const LOG_DOTS_SLOT_W: f32 = 22.0;
                let show_dots =
                    (viewable.is_some() && viewable == self.hover.log_row)
                        || menu_open
                        || kb_selected;
                let kebab: Element<'_, Message> = if show_dots {
                    crate::widgets::card_kebab_button(
                        OryxisColors::t().text_muted,
                        true,
                        Message::History(HistoryMessage::ShowSessionLogMenu(idx)),
                    )
                    .into()
                } else {
                    Space::new()
                        .width(Length::Fixed(LOG_DOTS_SLOT_W))
                        .height(Length::Fixed(22.0))
                        .into()
                };
                let _ = entry;
                crate::widgets::dir_row(vec![
                    text(ts)
                        .size(10)
                        .color(OryxisColors::t().text_muted)
                        .into(),
                    Space::new().width(8).into(),
                    kebab,
                ])
                .align_y(iced::Alignment::Center)
                .into()
            }
            // A saved conversation gets the same kebab as a session, with
            // its own menu (open / delete). The row click opens it.
            TimelineKind::Chat { idx, entry } => {
                let idx = *idx;
                let menu_open = matches!(
                    self.overlay.as_ref().map(|o| &o.content),
                    Some(crate::state::OverlayContent::ChatConversationActions(i)) if *i == idx
                );
                const LOG_DOTS_SLOT_W: f32 = 22.0;
                let show_dots = (viewable.is_some() && viewable == self.hover.log_row)
                    || menu_open
                    || kb_selected;
                let kebab: Element<'_, Message> = if show_dots {
                    crate::widgets::card_kebab_button(
                        OryxisColors::t().text_muted,
                        true,
                        Message::History(HistoryMessage::ShowChatConversationMenu(idx)),
                    )
                    .into()
                } else {
                    Space::new()
                        .width(Length::Fixed(LOG_DOTS_SLOT_W))
                        .height(Length::Fixed(22.0))
                        .into()
                };
                let _ = entry;
                crate::widgets::dir_row(vec![
                    text(ts)
                        .size(10)
                        .color(OryxisColors::t().text_muted)
                        .into(),
                    Space::new().width(8).into(),
                    kebab,
                ])
                .align_y(iced::Alignment::Center)
                .into()
            }
            TimelineKind::Failure(_) => text(ts)
                .size(10)
                .color(OryxisColors::t().text_muted)
                .into(),
        };

        let mut title_col = column![
            crate::widgets::dir_row(vec![
                text(display_label)
                    .size(13)
                    .color(OryxisColors::t().text_primary)
                    .into(),
                Space::new().width(8).into(),
                chip.into(),
            ])
            .align_y(iced::Alignment::Center),
            Space::new().height(2),
            text(subtitle)
                .size(11)
                .color(OryxisColors::t().text_muted),
        ]
        .width(Length::Fill)
        .align_x(crate::widgets::dir_align_x());
        // Content-search hit: the matched command / output excerpt,
        // so the row says WHY it survived the filter. Raw session
        // content, so Privacy Mode redacts it like failure messages.
        if let Some(hit) = content_hit {
            let hit_disp = if mask {
                crate::widgets::redact_for_display(
                    hit,
                    &self.privacy_terms(),
                    self.privacy_classes(),
                )
            } else {
                hit.to_string()
            };
            title_col = title_col.push(Space::new().height(2)).push(
                crate::widgets::dir_row(vec![
                    iced_fonts::lucide::search()
                        .size(10)
                        .color(OryxisColors::t().accent)
                        .into(),
                    Space::new().width(5).into(),
                    text(hit_disp)
                        .size(11)
                        .color(OryxisColors::t().text_secondary)
                        .into(),
                ])
                .align_y(iced::Alignment::Center),
            );
        }
        let card = container(
            crate::widgets::dir_row(vec![
                badge,
                Space::new().width(12).into(),
                title_col.into(),
                Space::new().width(12).into(),
                trailing,
            ])
            .align_y(iced::Alignment::Center),
        )
        .padding(Padding { top: 8.0, right: 16.0, bottom: 8.0, left: 16.0 })
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(if hovered {
                OryxisColors::t().bg_hover
            } else {
                OryxisColors::t().bg_surface
            })),
            border: Border {
                radius: Radius::from(8.0),
                ..Default::default()
            },
            ..Default::default()
        });

        match viewable {
            Some(log_id) => {
                // Right-click anywhere on the row opens the kebab menu
                // (app-wide card convention); left click still opens
                // the recording.
                // A conversation opens its own reader, not the recording
                // viewer; both are a plain left click on the row.
                let open = match &row.kind {
                    TimelineKind::Chat { .. } => {
                        Message::History(HistoryMessage::OpenChatConversation(log_id))
                    }
                    _ => Message::History(HistoryMessage::ViewSessionLog(log_id)),
                };
                let mut area = iced::widget::MouseArea::new(card)
                    .on_press(open)
                    .on_enter(Message::History(HistoryMessage::LogRowHovered(log_id)))
                    .on_exit(Message::History(HistoryMessage::LogRowUnhovered(log_id)))
                    .interaction(iced::mouse::Interaction::Pointer);
                match &row.kind {
                    TimelineKind::Session { idx, .. } => {
                        area = area.on_right_press(Message::History(
                            HistoryMessage::ShowSessionLogMenu(*idx),
                        ));
                    }
                    TimelineKind::Chat { idx, .. } => {
                        area = area.on_right_press(Message::History(
                            HistoryMessage::ShowChatConversationMenu(*idx),
                        ));
                    }
                    TimelineKind::Failure(_) => {}
                }
                let row_el: Element<'_, Message> = area.into();
                crate::widgets::select_ring_opt(
                    row_el,
                    8.0,
                    kb_selected.then(|| OryxisColors::t().accent),
                )
            }
            None => card.into(),
        }
    }
}

/// Small bordered header button for the session-log viewer and the
/// player header, matching the Close button's look: an icon,
/// optionally with a label, hover fill for feedback (the app-wide
/// button convention).
pub(super) fn viewer_header_btn<'a>(
    icon: Element<'a, Message>,
    label: Option<&'a str>,
    msg: Message,
) -> Element<'a, Message> {
    let mut items: Vec<Element<'a, Message>> = vec![icon];
    if let Some(label) = label {
        items.push(Space::new().width(6).into());
        items.push(
            text(label)
                .size(11)
                .font(iced::Font {
                    weight: iced::font::Weight::Bold,
                    ..iced::Font::new(crate::theme::SYSTEM_UI_FAMILY)
                })
                .color(OryxisColors::t().text_secondary)
                .into(),
        );
    }
    button(
        container(
            crate::widgets::dir_row(items).align_y(iced::Alignment::Center),
        )
        .center_y(Length::Fixed(24.0))
        .padding(Padding { top: 0.0, right: 12.0, bottom: 0.0, left: 12.0 }),
    )
    .on_press(msg)
    .style(|_, status| {
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
    })
    .into()
}

/// Pagination chevron button. Disabled state has no `on_press` and a
/// muted look so it reads as unclickable at the boundaries.
fn nav_btn<'a>(
    icon: iced::widget::Text<'a>,
    msg: Message,
    enabled: bool,
) -> Element<'a, Message> {
    let icon = icon.size(12).color(if enabled {
        OryxisColors::t().text_secondary
    } else {
        OryxisColors::t().text_muted
    });
    let mut b = button(
        container(icon)
            .center(Length::Fixed(24.0))
            .height(Length::Fixed(24.0))
            .width(Length::Fixed(28.0)),
    )
    .style(move |_, status| {
        let bg = if enabled {
            match status {
                BtnStatus::Hovered => Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                _ => Color::TRANSPARENT,
            }
        } else {
            Color::TRANSPARENT
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
    });
    if enabled {
        b = b.on_press(msg);
    }
    b.into()
}

//! SFTP tab lifecycle arms split out of `dispatch_sftp`: select /
//! close / confirm-close / pin / close-others, the tab context menu,
//! tab hover (reorder-drag live-slide) and the new-tab entry point.
//! Called from `handle_sftp`.

#![allow(clippy::result_large_err)]

use iced::Task;

use crate::app::{SftpMessage, Message, Oryxis};

impl Oryxis {
    pub(super) fn handle_sftp_tabs(
        &mut self,
        message: SftpMessage,
    ) -> Result<Task<Message>, SftpMessage> {
        match message {
            SftpMessage::SelectSftpTab(idx) => {
                if idx < self.sftp_tabs.len() {
                    self.focus_sftp_tab(idx);
                    self.active_tab = None;
                    self.active_view = crate::state::View::Sftp;
                    self.panels.burger_menu = false;
                    // Dormant pinned tab (restored at boot): re-mount its remote
                    // pane on first focus. Single-remote case (the common
                    // left=Local / right=Remote); a dual-remote tab re-mounts
                    // only its right pane here.
                    let reopen = self.sftp_tabs[idx].pending_reopen.take();
                    if let Some(crate::state::PinnedTabSpec::Sftp { left, right, .. }) = reopen {
                        use crate::state::{SftpPaneSide, SftpPaneSpec};
                        self.refresh_sftp_local(SftpPaneSide::Left);
                        // Re-mount every remote pane the tab had (both, for a
                        // server-to-server tab). Each side is dispatched
                        // separately so the mount pipeline targets it correctly.
                        let mut tasks = Vec::new();
                        for (side, spec) in
                            [(SftpPaneSide::Right, &right), (SftpPaneSide::Left, &left)]
                        {
                            if let SftpPaneSpec::Remote(id) = spec
                                && let Some(ci) = self.connections.iter().position(|c| c.id == *id)
                            {
                                tasks.push(Task::done(Message::Sftp(SftpMessage::SftpRemountPane(side, ci))));
                            }
                        }
                        if !tasks.is_empty() {
                            return Ok(Task::batch(tasks));
                        }
                        return Ok(Task::none());
                    }
                    self.refresh_sftp_local(crate::state::SftpPaneSide::Left);
                    self.refresh_sftp_local(crate::state::SftpPaneSide::Right);
                }
            }
            SftpMessage::CloseSftpTab(idx) => {
                self.overlay = None;
                // The same opt-in guard a terminal tab's X has (issue
                // #204): a mounted SSH session is a live session, and the
                // setting says "a tab or the app", not "a terminal tab".
                if self.prefs.confirm_close_session_tab
                    && self.sftp_tab_is_live(idx)
                    && let Some(tab) = self.sftp_tabs.get(idx)
                {
                    let id = tab.id;
                    let label = tab.custom_name.clone().unwrap_or_else(|| tab.label.clone());
                    self.error_dialog = Some(crate::state::ErrorDialog {
                        title: crate::i18n::t("close_session_title").to_string(),
                        body: crate::i18n::t("close_session_body").replacen("{name}", &label, 1),
                        link: None,
                        action: Some(crate::state::ErrorDialogAction {
                            label: crate::i18n::t("close_session_confirm").to_string(),
                            message: Box::new(Message::Sftp(
                                SftpMessage::CloseSftpTabLiveConfirmed(id),
                            )),
                            danger: true,
                        }),
                    });
                    return Ok(Task::none());
                }
                // Guard: an in-flight transfer or unsaved edit-session opens a
                // confirmation modal instead of closing outright.
                if self.sftp_tab_has_unsaved(idx) {
                    self.pending_sftp_close = Some(crate::state::PendingSftpClose::One(idx));
                } else {
                    // Remembered here rather than inside `close_sftp_tab`:
                    // the "Open terminal" morph closes the SFTP tab as its
                    // last step, and that tab did not die, it became the
                    // terminal tab beside it (issue #186).
                    self.remember_closed_sftp_tab(idx);
                    return Ok(self.close_sftp_tab(idx));
                }
            }
            SftpMessage::CloseSftpTabLiveConfirmed(id) => {
                let Some(idx) = self.sftp_tabs.iter().position(|t| t.id == id) else {
                    return Ok(Task::none());
                };
                // The unsaved-work guard still applies after the live-session
                // one: two questions, each asked once.
                if self.sftp_tab_has_unsaved(idx) {
                    self.pending_sftp_close = Some(crate::state::PendingSftpClose::One(idx));
                } else {
                    self.remember_closed_sftp_tab(idx);
                    return Ok(self.close_sftp_tab(idx));
                }
            }
            SftpMessage::ConfirmCloseSftpTab => {
                match self.pending_sftp_close.take() {
                    Some(crate::state::PendingSftpClose::One(idx)) => {
                        self.remember_closed_sftp_tab(idx);
                        return Ok(self.close_sftp_tab(idx));
                    }
                    Some(crate::state::PendingSftpClose::Others(idx)) => {
                        return Ok(self.close_other_sftp_tabs(idx));
                    }
                    Some(crate::state::PendingSftpClose::HybridSession(tab_id)) => {
                        return Ok(self.close_tab_sftp_session(tab_id));
                    }
                    None => {}
                }
            }
            SftpMessage::CancelCloseSftpTab => {
                self.pending_sftp_close = None;
            }
            SftpMessage::ToggleSftpTabPin(idx) => {
                if let Some(t) = self.sftp_tabs.get_mut(idx) {
                    t.pinned = !t.pinned;
                }
                self.overlay = None;
                // Persist so the pin (and its arranged order) survives a relaunch.
                self.persist_pinned_tabs();
            }
            SftpMessage::CloseOtherSftpTabs(idx) => {
                self.overlay = None;
                if idx >= self.sftp_tabs.len() {
                    return Ok(Task::none());
                }
                // Guard: if any tab we'd drop has an in-flight transfer or an
                // unsaved edit-session, confirm first (mirrors CloseSftpTab)
                // instead of silently discarding it.
                if self.other_sftp_tabs_have_unsaved(idx) {
                    self.pending_sftp_close = Some(crate::state::PendingSftpClose::Others(idx));
                } else {
                    return Ok(self.close_other_sftp_tabs(idx));
                }
            }
            SftpMessage::ShowSftpTabMenu(idx) => {
                self.overlay = Some(crate::state::OverlayState {
                    content: crate::state::OverlayContent::SftpTabActions(idx),
                    x: self.mouse_position.x,
                    y: self.mouse_position.y,
                });
            }
            SftpMessage::SftpTabHovered(idx) => {
                self.hover.sftp_tab = Some(idx);
                // Terminal / SFTP hover are mutually exclusive (one cursor).
                self.hover.tab = None;
                // Live-slide: while a drag is active, entering this SFTP tab
                // slides the dragged tab (terminal or SFTP) into its slot in
                // the unified `tab_order`.
                if let Some(drag) = self.tab_drag.filter(|d| d.active)
                    && let Some(target) = self.sftp_tabs.get(idx).map(|t| t.id)
                    && drag.from_id != target
                {
                    self.slide_tab_in_order(drag.from_id, target);
                }
                // An SFTP chip carries its X in a reserved trailing slot,
                // so it has no dwell of its own to wait for. It still
                // opens a hover episode: that is what retires the timer
                // of the session chip the cursor crossed over from.
                return Ok(self.arm_tab_close_dwell());
            }
            SftpMessage::SftpTabUnhovered(idx) => {
                self.hover.leave_sftp_tab(idx);
            }
            SftpMessage::NewSftpTab => {
                self.overlay = None;
                // Dismiss the new-tab picker too: SFTP is selectable from it.
                self.panels.new_tab_picker = false;
                // ...and the burger menu, the other entry point (its own
                // flag, so clearing `overlay` above isn't enough); without
                // this it lingers over the freshly-opened SFTP tab and the
                // host picker until an extra click.
                self.panels.burger_menu = false;
                self.open_new_sftp_tab();
                // Empty tab: open the host picker for the remote pane.
                self.sftp.picker_open = true;
                self.sftp.picker_target = crate::state::SftpPaneSide::Right;
            }
            m => return Err(m),
        }
        Ok(Task::none())
    }
}

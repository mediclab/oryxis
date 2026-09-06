//! Failed dials, per pane and for the whole progress card.
//!
//! Both clear the in-flight marker first: `ReconnectTab` is a no-op
//! while a dial is pending, so a failure that forgets this leaves
//! the tab unable to retry.

use super::*;

impl Oryxis {
    pub(super) fn handle_ssh_errors(&mut self, message: SshMessage) -> Task<Message> {
        match message {
            SshMessage::PaneConnectError(pane_id, msg) => {
                // The dial for this pane is over; drop the in-flight
                // marker so ReconnectTab works again (it is a no-op
                // while a dial is pending).
                if let Some(tab_idx) = self.pane_tab_index(pane_id)
                    && let Some(pane) = self.tabs[tab_idx].pane_by_id_mut(pane_id)
                {
                    pane.connecting = false;
                }
                // A dial that failed before `SshConnected` never consumed
                // its parked reuse key; drop it here so the pending map
                // doesn't hold it until the next disconnect sweeps it.
                self.pending_reuse_keys.remove(&pane_id);
                // Identity / key switch on a split-pane quick connect: the
                // error is the cancel we provoked, reconnect the same pane
                // in place with the mutated entry.
                if let Some(qid) = self.pending_auth_switch
                    && let Some(tab_idx) = self.pane_tab_index(pane_id)
                    && self.tabs[tab_idx].pane_by_id_mut(pane_id).is_some_and(|p| {
                        matches!(
                            p.origin,
                            crate::state::PaneOrigin::QuickHost(q) if q == qid
                        )
                    })
                {
                    self.pending_auth_switch = None;
                    if let Some(pane) = self.tabs[tab_idx].pane_by_id_mut(pane_id)
                        && let Ok(mut state) = pane.terminal.lock()
                    {
                        state.process(
                            b"\r\nRetrying with the selected identity...\r\n",
                        );
                    }
                    return self.spawn_ssh_for_pane_quick(qid, tab_idx, pane_id);
                }
                // Surface the failure inside the pane that was connecting.
                if let Some(pane) = self
                    .tabs
                    .iter()
                    .flat_map(|t| t.pane_grid.panes.values())
                    .find(|p| p.id == pane_id)
                    && let Ok(mut state) = pane.terminal.lock()
                {
                    state.process(format!("\r\nConnection failed: {msg}\r\n").as_bytes());
                }
                // In a split, the pane carries its own verdict (issue
                // #208): `restart_pane` cleared `ended` for the attempt,
                // and without this the failure would read as "nothing to
                // report", with no card, no dot and no way back but the
                // chord. A lone pane is the tab's business, below.
                if let Some(tab_idx) = self.pane_tab_index(pane_id)
                    && self.tabs[tab_idx].pane_grid.panes.len() > 1
                    && let Some(pane) = self.tabs[tab_idx].pane_by_id_mut(pane_id)
                {
                    pane.ended = true;
                }
                // A failed *in-place reconnect* (single-pane tab whose label
                // matches a saved host) must fall back to the "(disconnected)"
                // state so `AutoReconnectTick` keeps retrying up to
                // `max_reconnect_attempts`. Split tabs (>1 pane) share this
                // message but stay connected via their live sibling panes;
                // session-group tabs carry the group name (no matching host),
                // so neither gets relabeled.
                if let Some(tab_idx) = self.pane_tab_index(pane_id)
                    && self.tabs[tab_idx].pane_grid.panes.len() == 1
                    && !self.tabs[tab_idx].label.ends_with(" (disconnected)")
                {
                    let label = self.tabs[tab_idx].label.clone();
                    // Quick-connect hosts join the retry loop too: their
                    // entry resolves by label like a saved host.
                    if self.any_connection_by_label(&label).is_some() {
                        self.tabs[tab_idx].label = format!("{label} (disconnected)");
                    }
                }
                tracing::error!("pane SSH connect failed: {msg}");
            }
            SshMessage::SshError(pane_id, err) => {
                // A cancel provoked by the identity / key switch: retry with
                // the mutated entry instead of surfacing the failure. The
                // guard on the progress origin keeps an (unlikely) stale flag
                // from hijacking an unrelated connect's error; the pane id
                // scopes it to the dial this error actually belongs to.
                if let Some(qid) = self.pending_auth_switch
                    && self.connecting.as_ref().is_some_and(|p| {
                        p.origin == crate::state::ProgressOrigin::Quick(qid)
                            && p.pane_id == pane_id
                    })
                {
                    self.pending_auth_switch = None;
                    return self.update(Message::Ssh(SshMessage::SshRetry));
                }
                // A cancel provoked by "Edit host" mid-connect: the card is
                // already gone and the editor is open, so this error is
                // expected teardown noise. Requiring `connecting == None`
                // keeps a fresh connect's genuine error from being eaten.
                if self.pending_edit_cancel && self.connecting.is_none() {
                    self.pending_edit_cancel = false;
                    tracing::debug!("swallowing edit-host-provoked connect error: {err}");
                    return Task::none();
                }
                tracing::error!("SSH error: {}", err);
                // History entry keyed by the pane's own tab label, not the
                // current progress card's: with concurrent connections the
                // card may be tracking a different dial than this error.
                let pane_label = self
                    .pane_tab_index(pane_id)
                    .and_then(|i| self.tabs.get(i))
                    .map(|t| t.label.clone());
                if self.should_record_history()
                    && let Some(vault) = &self.vault {
                    let label = pane_label.as_deref().unwrap_or("unknown");
                    let entry = oryxis_core::models::log_entry::LogEntry::new(
                        label, label, oryxis_core::models::log_entry::LogEvent::Error, &err,
                    );
                    let _ = vault.add_log(&entry);
                }
                // Empty-agent diagnostics (B3): when the auth failure
                // touched the agent and the host's referenced key is a
                // security key, the almost-certain cause is that the sk-
                // identity was never added to the OS agent. Append the
                // localized hint so the fix is one line away.
                let err = {
                    let sk_pinned = pane_label
                        .as_ref()
                        .and_then(|l| self.any_connection_by_label(l))
                        .and_then(|c| {
                            c.key_id.or_else(|| {
                                c.identity_id.and_then(|iid| {
                                    self.identities
                                        .iter()
                                        .find(|i| i.id == iid)
                                        .and_then(|i| i.key_id)
                                })
                            })
                        })
                        .and_then(|kid| self.keys.iter().find(|k| k.id == kid))
                        .is_some_and(|k| k.algorithm.is_security_key());
                    if sk_pinned && err.to_lowercase().contains("agent") {
                        format!("{err}\n{}", crate::i18n::t("sk_agent_hint"))
                    } else {
                        err
                    }
                };
                // Mark the progress card failed ONLY when it is tracking
                // this pane's dial. A later tab's connect may have taken
                // over the card while this dial was still in flight; in
                // that case the failure belongs to the pane itself, so
                // surface it there instead.
                if self
                    .connecting
                    .as_ref()
                    .is_some_and(|p| p.pane_id == pane_id)
                {
                    if let Some(ref mut progress) = self.connecting {
                        progress.failed = true;
                        progress.logs.push((progress.step, format!("Error: {}", err)));
                        // The connect is dead, so a KBI modal parked on it is an
                        // orphan: its oneshot is gone (submits would vanish
                        // silently) and the progress card's KBI branch renders
                        // OVER the failure timeline, hiding the real error
                        // (blanket auth timeout with the 2FA form open). Drop it
                        // and answer any waiting bridge.
                        if self.pending_kbi_prompt.is_some() {
                            self.pending_kbi_prompt = None;
                            self.pending_kbi_quick = None;
                            self.kbi_inputs.clear();
                            if let Some(ref tx) = self.kbi_response_tx {
                                let _ = tx.try_send(None);
                            }
                        }
                    }
                } else if let Some(pane) = self
                    .tabs
                    .iter()
                    .flat_map(|t| t.pane_grid.panes.values())
                    .find(|p| p.id == pane_id)
                    && let Ok(mut state) = pane.terminal.lock()
                {
                    // Concurrent-connect fallback: no card tracks this
                    // dial anymore, so print the failure into the pane's
                    // own terminal (mirrors `PaneConnectError`).
                    state.process(format!("\r\nConnection failed: {err}\r\n").as_bytes());
                } else {
                    self.host_panel_error = Some(format!("SSH: {}", err));
                }
            }
            // The router sends only this family here, so anything
            // else is a grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}

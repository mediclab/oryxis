//! Who the host is and how you authenticate to it.
//!
//! Label, address, credentials, TOTP, the key or identity reference, and
//! the auth method that decides which of those the form even shows.

use super::*;

impl Oryxis {
    /// Re-resolve the host's disk key so the editor hint matches what a
    /// connect would offer. Called from the arms that can change the
    /// answer, never from `view()`: it reads (and parses) a file, and
    /// the view runs every frame.
    pub(super) fn editor_refresh_disk_key(&mut self) {
        self.editor_form.disk_key_status = oryxis_vault::resolve_disk_key(
            self.editor_form.use_disk_key,
            Some(self.editor_form.identity_file.as_str()),
        )
        .status();
    }

    pub(super) fn handle_editor_identity(&mut self, message: EditorMessage) -> Task<Message> {
        match message {
            EditorMessage::EditorLabelChanged(v) => { self.editor_form.label = v; self.editor_form.username_focused = false; }
            EditorMessage::EditorTagsChanged(v) => { self.editor_form.tags_text = v; }
            EditorMessage::EditorHostnameChanged(v) => { self.editor_form.hostname = v; self.editor_form.username_focused = false; }
            EditorMessage::EditorPortChanged(v) => { self.editor_form.port = v; self.editor_form.username_focused = false; }
            EditorMessage::EditorUsernameChanged(v) => {
                self.editor_form.username = v;
                self.editor_form.username_focused = true;
            }
            EditorMessage::EditorPasswordChanged(v) => {
                self.editor_form.username_focused = false;
                self.editor_form.password.set(v.into_inner());
            }
            EditorMessage::EditorTogglePasswordVisibility => {
                self.toggle_editor_secret(super::EditorSecret::Password);
            }
            EditorMessage::EditorTotpChanged(v) => {
                self.editor_form.username_focused = false;
                self.editor_form.totp_secret.set(v.into_inner());
            }
            EditorMessage::EditorToggleTotpVisibility => {
                self.toggle_editor_secret(super::EditorSecret::Totp);
            }
            EditorMessage::EditorUseTotpToggled => {
                self.editor_form.use_totp = !self.editor_form.use_totp;
            }
            EditorMessage::EditorUseDiskKeyToggled => {
                self.editor_form.use_disk_key = !self.editor_form.use_disk_key;
                self.editor_refresh_disk_key();
            }
            EditorMessage::EditorIdentityFileChanged(v) => {
                self.editor_form.username_focused = false;
                self.editor_form.identity_file = v;
                // Resolved per keystroke: the read only happens once the
                // path names an existing file, and watching the hint
                // turn into a real path is how the user knows they typed
                // the right one.
                self.editor_refresh_disk_key();
            }
            EditorMessage::EditorBrowseIdentityFile => {
                // Starts in `~/.ssh` so the common case is one click.
                let start = crate::ssh_config::default_config_path()
                    .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
                return Task::perform(
                    tokio::task::spawn_blocking(move || {
                        let mut dialog = rfd::FileDialog::new().set_title(crate::i18n::t("select_private_key_title"));
                        if let Some(dir) = start {
                            dialog = dialog.set_directory(dir);
                        }
                        dialog.pick_file().map(|p| p.display().to_string())
                    }),
                    |res| match res {
                        Ok(Some(path)) => {
                            Message::Editor(EditorMessage::EditorIdentityFileChanged(path))
                        }
                        // A cancelled dialog leaves the field alone.
                        _ => Message::NoOp,
                    },
                );
            }
            EditorMessage::EditorAuthMethodChanged(v) => {
                // Localized (or English) label -> enum, shared with the
                // Settings default-auth picker.
                self.editor_form.auth_method = crate::util::auth_method_from_label(&v);
                // Certificate lists only keys that carry a cert: drop a
                // selection that is no longer offerable and rebuild the
                // combo with the filtered (or restored) option list.
                if self.editor_form.auth_method == AuthMethod::Certificate
                    && let Some(sel) = self.editor_form.selected_key.as_deref()
                    && !self
                        .keys
                        .iter()
                        .any(|k| k.label == sel && k.certificate.is_some())
                {
                    self.editor_form.selected_key = None;
                }
                self.reset_editor_key_combo();
            }
            EditorMessage::EditorGroupChanged(v) => self.editor_form.group_name = v,
            EditorMessage::EditorKeyChanged(v) => {
                self.editor_form.selected_key = if v == "(none)" { None } else { Some(v) };
            }
            EditorMessage::EditorKeyComboOpened => {
                // The widget empties its own input on focus, so the
                // dropdown already opens on the full key list. All this
                // has to do is pick up a key added while the editor was
                // open, and ONLY when there is one: an unconditional
                // rebuild re-filters the menu down to the current pick
                // (see `refresh_combo`).
                let options = self.editor_key_options();
                Self::refresh_combo(&mut self.editor_key_combo, options);
            }
            EditorMessage::EditorIdentityChanged(v) => {
                self.editor_form.username_focused = false;
                if v == "(none)" {
                    self.editor_form.selected_identity = None;
                } else {
                    self.editor_form.selected_identity = Some(v);
                }
            }
            EditorMessage::EditorIconStyleChanged(v) => {
                // "" clears the override; anything else is normalized to
                // the known set so a stale UI value can't smuggle in a
                // string the renderer doesn't understand.
                self.editor_form.icon_style = match v.as_str() {
                    "circular" | "square" | "rounded" | "outline" | "initials" => Some(v),
                    _ => None,
                };
            }
            EditorMessage::EditorProtocolChanged(protocol) => {
                let prev = self.editor_form.protocol;
                if prev != protocol {
                    // Retarget the numeric port when the new protocol
                    // has a conventional one AND the field holds a
                    // number nobody typed on purpose (any protocol's
                    // conventional port, or nothing at all). A
                    // user-typed 2222 survives the switch untouched.
                    //
                    // Deliberately NOT "equals the previous protocol's
                    // default": Serial and Local have no port, so a hop
                    // through either broke that chain and left a 22
                    // sitting in a Telnet host.
                    if let Some(new_port) = protocol.default_port() {
                        let typed = self.editor_form.port.trim();
                        let untouched = typed.is_empty()
                            || typed.parse::<u16>().is_ok_and(
                                oryxis_core::models::connection::ConnectionProtocol::is_conventional_port,
                            );
                        if untouched {
                            self.editor_form.port = new_port.to_string();
                        }
                    }
                    // Materialize serial defaults the first time a host
                    // becomes Serial so the reduced form has values to
                    // show (9600 8N1).
                    if protocol == oryxis_core::models::connection::ConnectionProtocol::Serial
                        && self.editor_form.serial.is_none()
                    {
                        self.editor_form.serial =
                            Some(oryxis_core::models::serial::SerialParams::default());
                    }
                    self.editor_form.protocol = protocol;
                    // The Local form picks from the curated local-terminal
                    // list, which is scanned lazily (first open of the
                    // local-shell picker). Without this the picker would
                    // offer nothing at all on a fresh install, which is
                    // exactly when someone is creating their first local
                    // host. `RescanLocalTerminals` merges and persists;
                    // `LocalShellsDetected` would OPEN a shell as its
                    // continuation, so it is deliberately not used here.
                    if protocol == oryxis_core::models::connection::ConnectionProtocol::Local
                        && self.local_terminals.is_none()
                    {
                        self.editor_form.username_focused = false;
                        return Task::done(Message::Settings(
                            crate::app::SettingsMessage::RescanLocalTerminals,
                        ));
                    }
                }
                self.editor_form.username_focused = false;
            }
            EditorMessage::EditorAddressFamilyChanged(family) => {
                self.editor_form.address_family = family;
            }
            EditorMessage::EditorToggleTelnetTls => {
                self.editor_form.telnet_tls = !self.editor_form.telnet_tls;
                // Retarget the port the same way the protocol picker
                // does: `telnets` is 992 and plain Telnet is 23, and a
                // port the user typed themselves stays untouched.
                let (from, to) = if self.editor_form.telnet_tls { (23, 992) } else { (992, 23) };
                if self.editor_form.port.trim() == from.to_string() {
                    self.editor_form.port = to.to_string();
                }
                // Turning TLS off drops the escape with it: it means
                // nothing without TLS, and leaving it armed would make
                // a later re-enable skip verification silently.
                if !self.editor_form.telnet_tls {
                    self.editor_form.telnet_tls_insecure = false;
                }
            }
            EditorMessage::EditorToggleTelnetTlsInsecure => {
                self.editor_form.telnet_tls_insecure = !self.editor_form.telnet_tls_insecure;
            }
            EditorMessage::EditorToggleMosh => {
                self.editor_form.mosh_enabled = !self.editor_form.mosh_enabled;
                // The three settings below it are KEPT. Unlike the
                // Telnet certificate escape, they arm nothing: they are
                // facts about the host, and a server path somebody had
                // to look up should not have to be found again.
            }
            EditorMessage::EditorMoshServerPathChanged(v) => {
                self.editor_form.mosh_server_path = v;
            }
            EditorMessage::EditorMoshPortRangeChanged(v) => {
                self.editor_form.mosh_port_range = v;
            }
            EditorMessage::EditorMoshCommandChanged(v) => {
                self.editor_form.mosh_command = v;
            }
            EditorMessage::EditorLocalTerminalChanged(id) => {
                self.editor_form.local_terminal_id = id;
            }
            EditorMessage::EditorLocalCwdChanged(v) => {
                self.editor_form.local_cwd = v;
            }
            // Routed here by the parent; anything else is a
            // grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}

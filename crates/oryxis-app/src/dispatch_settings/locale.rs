//! Language and layout direction.
//!
//! Switching to a CJK language may need a font downloaded first, so the
//! change lands in two steps and the picker stays honest about which one
//! it is in.

use super::*;

impl Oryxis {
    pub(super) fn handle_settings_locale(
        &mut self,
        message: SettingsMessage,
    ) -> Result<Task<Message>, SettingsMessage> {
        match message {
            SettingsMessage::LanguageChanged(token) => {
                use crate::i18n::Language;
                // Token-as-value from the picker: "auto" follows the
                // OS locale, anything else is a concrete language code.
                let lang = if token == "auto" {
                    crate::i18n::detect_os_language()
                } else {
                    Language::from_code(&token)
                };
                self.prefs.language_choice = if token == "auto" {
                    token
                } else {
                    // Persist the canonical code (`from_code` may have
                    // normalized an unknown token to English).
                    lang.code().to_string()
                };
                Language::set_active(lang);
                if let Some(vault) = &self.vault {
                    let _ = vault
                        .set_setting("language", &self.prefs.language_choice);
                }
                // Switching to a CJK language pulls its font on
                // demand (once per session). Show a hint while it
                // downloads; a cached font loads silently.
                if let Some(code) = crate::fonts::asset_code(lang)
                    && !self.loaded_cjk_fonts.contains(code)
                {
                    self.loaded_cjk_fonts.insert(code.to_string());
                    // Under offline mode the task answers at once with
                    // its own toast; a "downloading" hint before it
                    // would promise what is not going to happen.
                    if !crate::fonts::is_language_cached(lang) && !crate::offline::is_on() {
                        self.set_toast(
                            crate::i18n::t("cjk_font_downloading").to_string(),
                        );
                    }
                    return Ok(crate::fonts::ensure_task(lang));
                }
            }
            SettingsMessage::CjkFontReady(code, result) => match result {
                Ok(bytes) => {
                    // Clear the "downloading" hint and register the font
                    // with the iced font system so cosmic-text can fall
                    // back to it. `iced::font::Error` is uninhabited, so
                    // the load result is discarded.
                    self.toast = None;
                    return Ok(iced::font::load(bytes).discard());
                }
                Err(e) => {
                    tracing::warn!(
                        target = "oryxis::fonts",
                        lang = %code,
                        error = %e,
                        "CJK font not loaded; using system fallback"
                    );
                    // Drop the guard so a later switch (or the offline
                    // switch going off) can retry.
                    self.loaded_cjk_fonts.remove(&code);
                    // The font that cannot heal says so: under offline
                    // mode the miss is the mode's doing, not the
                    // network's, and the toast names the switch.
                    let key = match e {
                        crate::fonts::FetchError::Offline => "cjk_font_offline",
                        crate::fonts::FetchError::Other(_) => "cjk_font_failed",
                    };
                    self.set_toast(crate::i18n::t(key).to_string());
                    return Ok(Task::perform(
                        async {
                            tokio::time::sleep(
                                std::time::Duration::from_millis(2600),
                            )
                            .await;
                        },
                        |_| Message::ToastClear,
                    ));
                }
            },
            SettingsMessage::LayoutDirectionChanged(name) => {
                use crate::i18n::{t, LayoutDirection};
                // Match against the *localized* label since that's what
                // the pick_list emits; keys live on the enum so the
                // mapping survives language switches.
                if let Some(dir) = LayoutDirection::ALL
                    .iter()
                    .find(|d| t(d.label_key()) == name)
                {
                    LayoutDirection::set_active(*dir);
                    self.persist_setting("layout_direction", dir.code());
                }
            }
            m => return Err(m),
        }
        Ok(Task::none())
    }
}

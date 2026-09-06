//! History timeline + session-log recordings: paging, view/menu, delete, and .cast/transcript/GIF exports, wrapped by [`crate::messages::Message::History`]. Handled by `Oryxis::handle_history`.

use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum HistoryMessage {
    RequestClearHistory,
    CancelClearHistory,
    ClearLogs,
    LogsPageNext,
    LogsPagePrev,
    ViewSessionLog(Uuid),
    /// Open the kebab menu on a History session row.
    ShowSessionLogMenu(usize),
    /// Kebab / right-click menu of a saved AI conversation row.
    ShowChatConversationMenu(usize),
    /// Open a saved conversation in the read-only reader. Loads its turns
    /// from the vault; a conversation is never resumed, only re-read (the
    /// terminal it was held against is long gone).
    OpenChatConversation(Uuid),
    /// Close the conversation reader.
    CloseChatConversation,
    /// Ask before deleting a saved conversation; the confirm dialog's
    /// action carries `DeleteChatConversation`.
    RequestDeleteChatConversation(usize),
    DeleteChatConversation(usize),
    /// Export a recorded session as an asciicast v2 `.cast` file
    /// (replayable in the asciinema player). Output-only by design.
    ExportSessionCast(Uuid),
    /// Export a recorded session as a plain-text transcript (ANSI
    /// resolved and stripped by the same renderer the viewer uses).
    ExportSessionTranscript(Uuid),
    /// Export only the commands typed during a recorded session (the
    /// 'c' chunks) as a plain-text file.
    ExportSessionCommands(Uuid),
    /// Render a recorded session to an animated GIF via the
    /// `oryxis-gif` plugin (downloaded on first use). Opens the plugin
    /// install modal when the binary isn't present yet and resumes the
    /// export after the install.
    ExportSessionGif(Uuid),
    /// Outcome of a GIF render: `None` = save dialog dismissed (no
    /// toast), `Some(Ok(path))` / `Some(Err(cause))` otherwise.
    GifExportFinished(Option<Result<String, String>>),
    CloseSessionLogView,
    /// Switch the open transcript between the faithful replay and the
    /// linear dump (the header toggle). Rebuilds the viewer from the
    /// vault in the other mode.
    ToggleSessionViewerMode,
    /// Right-click context menu over the transcript viewer body (scheme
    /// = Menu): window-absolute x/y and the selection captured by the
    /// widget at right-click. Read-only, so it only offers copy actions.
    ShowSessionViewerContextMenu(f32, f32, Option<String>),
    /// Copy the whole transcript from the viewer's emulator to the
    /// clipboard (the "Copy All" item on that context menu).
    SessionViewerCopyAll,
    /// Toggle the viewer-header `...` menu (session-log actions minus
    /// Play, which the viewer offers as its own header button).
    ShowSessionLogViewerMenu(usize),
    /// Ask for confirmation before deleting one recording; the
    /// dialog's action carries `DeleteSessionLog`.
    RequestDeleteSessionLog(usize),
    /// Carries the recording's id, not its row: the list is reloaded on
    /// every `SshDisconnected` (a reconnecting tab lands a new row above
    /// the page while the dialog is up), and a row index resolved at
    /// confirm time would delete the neighbour.
    DeleteSessionLog(uuid::Uuid),
    /// Hover tracking for clickable session rows in the Logs view.
    LogRowHovered(Uuid),
    LogRowUnhovered(uuid::Uuid),
    #[allow(dead_code)]
    ClearSessionLogs,
    #[allow(dead_code)]
    SessionLogsPageNext,
    #[allow(dead_code)]
    SessionLogsPagePrev,
    /// Copy the canonical ssh:// URL of the host at this index (card
    /// context-menu action).
    CopyHostSshUrl(usize),
    /// Send the Wake-on-LAN magic packet to the host at this index
    /// (card context-menu action, shown only when a MAC is stored).
    WakeOnLan(usize),
    /// Toggle the "search in session content" chip inside the History
    /// search field: matches commands + recorded output on top of the
    /// label/hostname filter.
    SearchContentToggled,
    /// Debounce timer for the content search; carries the generation
    /// it was armed for, so a newer keystroke silently retires it.
    SearchContentDebounce(u64),
    /// One session's output scan finished: the matched excerpt (None =
    /// no match) for `log_id`, tagged with the generation that owns
    /// the scan. A stale generation drops the result AND the pump.
    SearchContentScanned {
        generation: u64,
        log_id: Uuid,
        snippet: Option<String>,
    },
    /// Toggle the History-toolbar tag-filter dropdown.
    ShowHistoryTagFilterMenu,
    /// Toggle one tag in the History tag filter (multi-select, menu
    /// stays open like the dashboard's).
    ToggleHistoryTagFilterTag(String),
    /// Clear the History tag filter and close the dropdown.
    ClearHistoryTagFilter,
}

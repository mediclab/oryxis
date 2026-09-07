//! Terminal-sidebar command-history capture, re-run and export, wrapped by [`crate::messages::Message::CommandHistory`]. Handled by `Oryxis::handle_command_history`.

use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum CommandHistoryMessage {
    HistoryCardHovered(usize),
    HistoryCardUnhovered(usize),
    /// Re-run a captured command in the active terminal (+ Enter).
    RunHistoryCommand(Uuid),
    /// Insert a captured command WITHOUT the trailing newline.
    PasteHistoryCommand(Uuid),
    /// Ask before removing a captured command (routes through the shared
    /// confirm dialog; a lone misclick on the hover trash silently wiped
    /// a host's only entry once, live QA 2026-07-03).
    RequestDeleteHistoryCommand(Uuid),
    /// Remove one captured command from the host's history (confirmed).
    DeleteHistoryCommand(Uuid),
    /// Filter text for the sidebar History tab's search field (distinct
    /// from `HistorySearchChanged`, which filters the session-logs view).
    CmdHistorySearchChanged(String),
    /// Save the focused host's captured commands to a plain-text file
    /// (save dialog; offline reference / support sharing).
    ExportCommandHistory,
    /// Outcome of the export: `Ok(path)` shows a toast, `Err` a warning.
    CommandHistoryExported(Result<String, String>),
    /// Settings > Terminal: live-append captured commands to per-host
    /// text files.
    ToggleCommandHistoryFile,
    /// Pick the folder the per-host command logs are written into.
    PickCommandHistoryDir,
    /// Folder chosen (or dialog dismissed with `None`).
    CommandHistoryDirPicked(Option<String>),
    /// Reset the command-log folder to the default
    /// (`~/.oryxis/command-history/`).
    ClearCommandHistoryDir,
}

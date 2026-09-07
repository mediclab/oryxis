//! The plain-text mirror of a live session recording (issue #187).
//!
//! Everything Oryxis records goes into the vault encrypted, and History
//! exports it afterwards. What this adds is the other half of the ask:
//! a file on disk that grows WHILE the session runs, for tailing from
//! another window and for handing to somebody who does not have the
//! vault.
//!
//! Two rules make it safe to offer at all. It is off by default and it
//! is NOT its own capture: the bytes come from the same flush that
//! feeds the vault, so a host set to never record produces no file
//! either, and the redaction that scrubs the stored chunk is the same
//! pass that reaches the disk. And what lands there is what a person
//! read, not the wire: the chunk goes through the linear ANSI renderer
//! (`ansi_render`, the transcript export's own pipeline), so a progress
//! bar is one line per flush rather than a thousand escape sequences.
//! Per flush, because each chunk is rendered on its own: a redraw that
//! reaches back into the previous chunk cannot, so a bar that repaints
//! in place for ten seconds lands as a handful of lines, not one.
//!
//! The file is written by ONE thread of its own (`MirrorWriter`), never
//! on the UI thread: a flush runs inside `update()`, and a disk that
//! stalls (a network folder, a sleeping drive) would stall every frame
//! with it. One thread rather than a blocking task per flush because
//! two tasks for one file can land out of order and one thread cannot.
//! The rendering goes with it, so the UI thread hands over bytes and a
//! palette and nothing else. A failure comes back on a second channel
//! and is read at the next flush, so the mirror is switched off for the
//! recording it failed on within a tick, the same as before.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use uuid::Uuid;

use crate::app::Oryxis;
use crate::state::PaneOrigin;

/// One flushed chunk on its way to a mirror file.
pub(crate) struct MirrorJob {
    pub log_id: Uuid,
    pub path: PathBuf,
    /// The scrubbed bytes the vault accepted, rendered on the thread.
    pub bytes: Vec<u8>,
    pub palette: oryxis_terminal::TerminalPalette,
}

enum MirrorRequest {
    Write(Box<MirrorJob>),
    /// Answer once every job queued before it has been written.
    Drain(mpsc::Sender<()>),
}

/// The mirror's writer thread, started on the first job and kept for
/// the life of the process.
pub(crate) struct MirrorWriter {
    jobs: mpsc::Sender<MirrorRequest>,
    failures: mpsc::Receiver<Uuid>,
}

impl MirrorWriter {
    fn start() -> Self {
        let (jobs, job_rx) = mpsc::channel::<MirrorRequest>();
        let (failure_tx, failures) = mpsc::channel::<Uuid>();
        // A thread that cannot start leaves `jobs` with no receiver, so
        // every `enqueue` from then on answers false and the flush
        // treats the refusal as a failed write for that recording.
        let spawned = std::thread::Builder::new()
            .name("session-log-mirror".into())
            .spawn(move || {
                while let Ok(request) = job_rx.recv() {
                    match request {
                        MirrorRequest::Write(job) => {
                            if let Err(e) = append_mirror_file(&job.path, &job.bytes, &job.palette) {
                                tracing::warn!(
                                    "session log file append failed for {}: {e}",
                                    job.path.display()
                                );
                                let _ = failure_tx.send(job.log_id);
                            }
                        }
                        MirrorRequest::Drain(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("session log mirror thread did not start: {e}");
        }
        Self { jobs, failures }
    }

    /// Queue one chunk. Never blocks the caller. `false` when the
    /// thread is gone, which the caller treats as that recording's
    /// mirror having failed.
    pub(crate) fn enqueue(&self, job: MirrorJob) -> bool {
        let accepted = self.jobs.send(MirrorRequest::Write(Box::new(job))).is_ok();
        if !accepted {
            tracing::warn!("session log mirror thread is gone; a chunk was not mirrored");
        }
        accepted
    }

    /// The recordings whose file could not be written since the last
    /// call, for the caller to switch the mirror off on.
    pub(crate) fn take_failures(&self) -> Vec<Uuid> {
        let mut failed: Vec<Uuid> = self.failures.try_iter().collect();
        failed.dedup();
        failed
    }

    /// Wait until everything queued so far is on disk, or `timeout`
    /// passes: the exit doors call this before `process::exit`, which
    /// runs no destructor and would drop the tail of every mirror.
    pub(crate) fn drain(&self, timeout: std::time::Duration) {
        let (ack_tx, ack_rx) = mpsc::channel::<()>();
        if self.jobs.send(MirrorRequest::Drain(ack_tx)).is_err() {
            return;
        }
        if ack_rx.recv_timeout(timeout).is_err() {
            tracing::warn!("session log mirror did not drain within {timeout:?}");
        }
    }
}

impl Oryxis {
    /// The folder the plain-text session logs live in: the configured
    /// setting, or `~/.oryxis/session-logs/` by default. Sibling of
    /// `command_history_dir`, and deliberately a different folder: one
    /// holds command lines, the other whole sessions.
    pub(crate) fn session_log_file_dir(&self) -> PathBuf {
        match &self.prefs.session_log_file_dir {
            Some(dir) => PathBuf::from(dir),
            None => oryxis_core::paths::oryxis_dir()
                .unwrap_or_else(|| PathBuf::from(".").join(".oryxis"))
                .join("session-logs"),
        }
    }

    /// Where THIS recording's mirror goes: `<label>-<date>-<id8>.txt`.
    ///
    /// The date is what makes a folder of these readable at a glance and
    /// the id is what keeps two sessions opened in the same second
    /// apart, so both are in the name. It is computed once per
    /// recording and parked on the pane, because a name recomputed per
    /// flush would move the file mid-session.
    pub(crate) fn session_log_file_path(&self, log_id: &Uuid, origin: &PaneOrigin) -> PathBuf {
        let label = match origin {
            PaneOrigin::Host(id) => self
                .connections
                .iter()
                .find(|c| c.id == *id)
                .map(|c| c.label.clone()),
            PaneOrigin::QuickHost(id) => {
                self.quick_connects.get(id).map(|e| e.conn.label.clone())
            }
            PaneOrigin::Local(spec) => Some(spec.label.clone()),
            PaneOrigin::Ephemeral => None,
        };
        let stem = crate::util::sanitize_file_stem(
            label.as_deref().filter(|l| !l.trim().is_empty()).unwrap_or("session"),
        );
        let id8 = log_id.simple().to_string();
        self.session_log_file_dir().join(format!(
            "{stem}-{}-{}.txt",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            &id8[..8]
        ))
    }

    /// The mirror's writer thread, started on first use.
    pub(crate) fn mirror_writer(&mut self) -> &MirrorWriter {
        self.mirror_writer.get_or_insert_with(MirrorWriter::start)
    }

    /// Wait for the queued mirror writes before an exit door closes the
    /// process; a no-op when nothing was ever mirrored.
    pub(crate) fn mirror_writer_drain(&self, timeout: std::time::Duration) {
        if let Some(writer) = &self.mirror_writer {
            writer.drain(timeout);
        }
    }
}

/// Append one flushed chunk to the mirror, creating the folder and the
/// file on the first call. Runs on the writer thread.
///
/// Owner-only on unix, like the command log and the vault file itself:
/// the content is plaintext by design, which is no reason to let the
/// other accounts on the machine read a session. The file is 0600; a
/// folder is 0700 only when THIS call creates it. A folder the user
/// picked keeps the mode they gave it: it may be shared on purpose, and
/// a chmod on every flush would take that away from every other account
/// in silence. Windows has no mode to set, so the file takes the
/// folder's ACL, which is the user's to choose.
fn append_mirror_file(
    path: &Path,
    data: &[u8],
    palette: &oryxis_terminal::TerminalPalette,
) -> std::io::Result<()> {
    let text: String = crate::ansi_render::render(data, palette)
        .iter()
        .map(|s| s.text.as_str())
        .collect();
    if text.is_empty() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
    }
    let fresh = !path.exists();
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    if fresh {
        // A header, once: a file found months later has to say what it
        // is a recording OF, and the plain warning belongs on the
        // artifact rather than only in the setting that made it. The
        // time is the mirror's own start (the first flush, which can be
        // mid-session when the toggle was turned on late), not the
        // session's; the vault row holds that one.
        writeln!(
            file,
            "# Oryxis session log (plain-text mirror), mirror started {}\n# Not encrypted.\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        )?;
    }
    file.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(log_id: Uuid, path: PathBuf) -> MirrorJob {
        MirrorJob {
            log_id,
            path,
            bytes: b"hello\r\n".to_vec(),
            palette: oryxis_terminal::TerminalPalette::default(),
        }
    }

    #[test]
    fn a_failed_write_is_reported_once_and_only_after_it_happened() {
        let dir = tempfile::tempdir().unwrap();
        // A parent that is a FILE cannot be created as a directory, so
        // the append fails the way an unplugged folder does.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let writer = MirrorWriter::start();
        let bad = Uuid::new_v4();
        let good = Uuid::new_v4();
        assert!(writer.enqueue(job(bad, blocker.join("session.txt"))));
        assert!(writer.enqueue(job(bad, blocker.join("session.txt"))));
        assert!(writer.enqueue(job(good, dir.path().join("ok.txt"))));
        writer.drain(std::time::Duration::from_secs(5));
        assert_eq!(writer.take_failures(), vec![bad]);
        assert!(writer.take_failures().is_empty());
        assert!(dir.path().join("ok.txt").exists());
    }

    #[test]
    fn a_writer_with_no_thread_refuses_the_job_on_the_spot() {
        let (jobs, job_rx) = mpsc::channel::<MirrorRequest>();
        let (_failure_tx, failures) = mpsc::channel::<Uuid>();
        drop(job_rx);
        let writer = MirrorWriter { jobs, failures };
        assert!(!writer.enqueue(job(Uuid::new_v4(), PathBuf::from("/nowhere/x.txt"))));
        assert!(writer.take_failures().is_empty());
        // Draining a dead writer returns at once rather than waiting out
        // the timeout.
        let t = std::time::Instant::now();
        writer.drain(std::time::Duration::from_secs(5));
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }
}

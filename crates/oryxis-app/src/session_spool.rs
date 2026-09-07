//! Where a recording goes while the vault is soft-locked.
//!
//! The soft lock zeroizes the master key and keeps the live sessions
//! (`SoftLockVault`), so their output keeps arriving with nowhere to be
//! stored: the vault refuses an append, and the flush tick is unmounted
//! for that reason. The buffers used to sit on the panes until unlock,
//! without bound, so a chatty session left running overnight under the
//! idle lock grew the process by everything it printed.
//!
//! This is the bound. Under lock, the flush spools the rows it would
//! have written to the vault into `~/.oryxis/runtime/spool/<log>.spool`,
//! each row sealed under an [`oryxis_vault::EphemeralKey`] that lives in
//! this process alone: the file is as readable at rest as the process's
//! memory was, which is the posture the lock already accepts, and it is
//! unreadable to the next launch, which sweeps the folder. The drain
//! runs on the first flush after unlock and replays every file into the
//! vault ahead of whatever the panes hold then, `End` rows included, so
//! a recording that ended under lock is stamped ended too.
//!
//! A session closed under the lock is fine: its spool has no pane any
//! more and is drained all the same. What is lost is a spool whose
//! PROCESS exits before the unlock (the window closed under the lock):
//! only that process could read it. Unlocking first keeps it, which is
//! what the lock screen is for.

use std::io::{Read, Write};
use std::path::PathBuf;

use uuid::Uuid;

use crate::dispatch_terminal::PendingSessionRow;

const KIND_CHUNK: u8 = 0;
const KIND_RESIZE: u8 = 1;
const KIND_END: u8 = 2;

/// The spool folder plus the key its files are sealed under.
pub(crate) struct SessionSpool {
    key: oryxis_vault::EphemeralKey,
    dir: PathBuf,
}

impl SessionSpool {
    /// The folder every spool lives in, `None` with no home to put it in.
    fn dir() -> Option<PathBuf> {
        Some(oryxis_core::paths::oryxis_dir()?.join("runtime").join("spool"))
    }

    /// Open the spool for this process: the folder (owner-only on unix)
    /// and a fresh key. `None` when either cannot be had, which the
    /// caller reads as "keep the rows in memory", the old behaviour.
    pub(crate) fn open() -> Option<Self> {
        let dir = Self::dir()?;
        Self::open_in(dir)
    }

    fn open_in(dir: PathBuf) -> Option<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        if let Err(e) = builder.create(&dir) {
            tracing::warn!("session spool folder {}: {e}", dir.display());
            return None;
        }
        let key = match oryxis_vault::EphemeralKey::generate() {
            Ok(key) => key,
            Err(e) => {
                tracing::warn!("session spool key: {e}");
                return None;
            }
        };
        Some(Self { key, dir })
    }

    fn path_for(&self, log_id: Uuid) -> PathBuf {
        self.dir.join(format!("{}.spool", log_id.simple()))
    }

    /// Append `rows` to the recording's spool, in order. One write for
    /// the batch, so a crash mid-batch leaves at most a torn tail, which
    /// the reader stops at.
    pub(crate) fn append(&self, log_id: Uuid, rows: &[PendingSessionRow]) -> std::io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut out: Vec<u8> = Vec::new();
        for row in rows {
            let blob = self
                .key
                .seal(&encode_row(row))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            out.extend_from_slice(&(blob.len() as u32).to_be_bytes());
            out.extend_from_slice(&blob);
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(self.path_for(log_id))?;
        file.write_all(&out)?;
        file.flush()
    }

    /// Take every spooled recording: its rows in the order they were
    /// spooled, and the file gone. A file that stops decoding (a torn
    /// tail, a blob from another key) yields what it had up to there;
    /// the file is removed either way, since nothing later could read
    /// it.
    pub(crate) fn drain_all(&self) -> Vec<(Uuid, Vec<PendingSessionRow>)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut drained = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(log_id) = path
                .extension()
                .filter(|ext| *ext == "spool")
                .and_then(|_| path.file_stem())
                .and_then(|stem| stem.to_str())
                .and_then(|stem| Uuid::parse_str(stem).ok())
            else {
                continue;
            };
            let rows = match std::fs::File::open(&path) {
                Ok(mut file) => {
                    let mut data = Vec::new();
                    match file.read_to_end(&mut data) {
                        Ok(_) => self.decode_all(&data, &path),
                        Err(e) => {
                            tracing::warn!("session spool {}: {e}", path.display());
                            Vec::new()
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("session spool {}: {e}", path.display());
                    Vec::new()
                }
            };
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!("session spool {} not removed: {e}", path.display());
            }
            if !rows.is_empty() {
                drained.push((log_id, rows));
            }
        }
        drained
    }

    fn decode_all(&self, data: &[u8], path: &std::path::Path) -> Vec<PendingSessionRow> {
        let mut rows = Vec::new();
        let mut rest = data;
        while let Some((len, after)) = rest.split_first_chunk::<4>() {
            let len = u32::from_be_bytes(*len) as usize;
            let Some((blob, after)) = after.split_at_checked(len) else {
                tracing::debug!("session spool {}: torn tail dropped", path.display());
                break;
            };
            let row = self.key.open(blob).ok().and_then(|frame| decode_row(&frame));
            let Some(row) = row else {
                tracing::warn!("session spool {}: a row did not decode; stopping there", path.display());
                break;
            };
            rows.push(row);
            rest = after;
        }
        rows
    }

    /// Delete every spool a previous process left: sealed under a key
    /// that died with it, they can only be removed. Runs at boot.
    pub(crate) fn sweep_stale() {
        let Some(dir) = Self::dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut swept = 0usize;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "spool")
                && std::fs::remove_file(&path).is_ok()
            {
                swept += 1;
            }
        }
        if swept > 0 {
            tracing::info!("swept {swept} session spool file(s) from a previous run");
        }
    }
}

fn encode_row(row: &PendingSessionRow) -> Vec<u8> {
    let mut out = Vec::new();
    match row {
        PendingSessionRow::Chunk(offset_ms, bytes) => {
            out.push(KIND_CHUNK);
            match offset_ms {
                Some(ms) => {
                    out.push(1);
                    out.extend_from_slice(&ms.to_be_bytes());
                }
                None => {
                    out.push(0);
                    out.extend_from_slice(&0i64.to_be_bytes());
                }
            }
            out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        PendingSessionRow::Resize(ms, cols, rows) => {
            out.push(KIND_RESIZE);
            out.extend_from_slice(&ms.to_be_bytes());
            out.extend_from_slice(&cols.to_be_bytes());
            out.extend_from_slice(&rows.to_be_bytes());
        }
        PendingSessionRow::End => out.push(KIND_END),
    }
    out
}

fn decode_row(frame: &[u8]) -> Option<PendingSessionRow> {
    let (kind, rest) = frame.split_first()?;
    match *kind {
        KIND_CHUNK => {
            let (has_offset, rest) = rest.split_first()?;
            let (ms, rest) = rest.split_first_chunk::<8>()?;
            let (len, rest) = rest.split_first_chunk::<4>()?;
            let len = u32::from_be_bytes(*len) as usize;
            let bytes = rest.get(..len)?;
            let offset = (*has_offset == 1).then(|| i64::from_be_bytes(*ms));
            Some(PendingSessionRow::Chunk(offset, bytes.to_vec()))
        }
        KIND_RESIZE => {
            let (ms, rest) = rest.split_first_chunk::<8>()?;
            let (cols, rest) = rest.split_first_chunk::<2>()?;
            let (rows, _) = rest.split_first_chunk::<2>()?;
            Some(PendingSessionRow::Resize(
                i64::from_be_bytes(*ms),
                u16::from_be_bytes(*cols),
                u16::from_be_bytes(*rows),
            ))
        }
        KIND_END => Some(PendingSessionRow::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spool() -> (tempfile::TempDir, SessionSpool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let spool = SessionSpool::open_in(dir.path().join("spool")).expect("spool");
        (dir, spool)
    }

    fn same(a: &PendingSessionRow, b: &PendingSessionRow) -> bool {
        match (a, b) {
            (PendingSessionRow::Chunk(x, xb), PendingSessionRow::Chunk(y, yb)) => x == y && xb == yb,
            (PendingSessionRow::Resize(a1, a2, a3), PendingSessionRow::Resize(b1, b2, b3)) => {
                a1 == b1 && a2 == b2 && a3 == b3
            }
            (PendingSessionRow::End, PendingSessionRow::End) => true,
            _ => false,
        }
    }

    /// Every row kind comes back as it went, in order, and the drain
    /// leaves nothing behind for a second drain to find.
    #[test]
    fn rows_round_trip_in_order_and_the_file_goes() {
        let (_dir, spool) = spool();
        let id = Uuid::new_v4();
        let rows = vec![
            PendingSessionRow::Resize(0, 120, 40),
            PendingSessionRow::Chunk(Some(12), b"hello\r\n".to_vec()),
            PendingSessionRow::Chunk(None, b"plain".to_vec()),
            PendingSessionRow::End,
        ];
        spool.append(id, &rows[..2]).expect("append");
        spool.append(id, &rows[2..]).expect("append");
        let drained = spool.drain_all();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, id);
        assert_eq!(drained[0].1.len(), rows.len());
        for (got, want) in drained[0].1.iter().zip(&rows) {
            assert!(same(got, want), "a row changed on the way");
        }
        assert!(spool.drain_all().is_empty(), "the file survived its drain");
    }

    /// Bytes on disk are sealed: the payload is not there to read.
    #[test]
    fn the_file_holds_no_plaintext() {
        let (_dir, spool) = spool();
        let id = Uuid::new_v4();
        spool
            .append(id, &[PendingSessionRow::Chunk(Some(1), b"secret output".to_vec())])
            .expect("append");
        let raw = std::fs::read(spool.path_for(id)).expect("read");
        assert!(!raw.windows(13).any(|w| w == b"secret output"));
    }

    /// A torn tail (a crash mid-write) costs the torn row only.
    #[test]
    fn a_torn_tail_keeps_the_rows_before_it() {
        let (_dir, spool) = spool();
        let id = Uuid::new_v4();
        spool
            .append(id, &[PendingSessionRow::Chunk(Some(1), b"first".to_vec())])
            .expect("append");
        spool
            .append(id, &[PendingSessionRow::Chunk(Some(2), b"second".to_vec())])
            .expect("append");
        let path = spool.path_for(id);
        let mut raw = std::fs::read(&path).expect("read");
        raw.truncate(raw.len() - 5);
        std::fs::write(&path, raw).expect("write");
        let drained = spool.drain_all();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].1.len(), 1);
        assert!(same(&drained[0].1[0], &PendingSessionRow::Chunk(Some(1), b"first".to_vec())));
    }

    /// Another process's spool (another key) yields nothing and is
    /// removed, which is what the boot sweep does without a key at all.
    #[test]
    fn a_foreign_file_is_dropped_not_decoded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = SessionSpool::open_in(dir.path().join("spool")).expect("spool");
        let b = SessionSpool::open_in(dir.path().join("spool")).expect("spool");
        let id = Uuid::new_v4();
        a.append(id, &[PendingSessionRow::End]).expect("append");
        assert!(b.drain_all().is_empty());
        assert!(!a.path_for(id).exists(), "the foreign file was kept");
    }
}

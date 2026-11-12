//! Write-ahead log.
//!
//! Each record is `[len: u32 LE][crc32: u32 LE][payload]`, where the payload is
//! `postcard((sequence, entry))`. Appends are fsynced before returning, so an
//! acknowledged write survives a crash.
//!
//! On open, a torn record at the end of the file (a partial write from a crash)
//! is truncated away. Scanning also stops at the first record whose checksum
//! fails, and everything after it is discarded. This is safe for a single
//! append-only writer, but it cannot tell corruption in the middle of the log
//! apart from a torn tail.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tpt_search_core::{Document, DocumentId, StorageError};

const HEADER_LEN: usize = 8;

/// A logical operation recorded in the log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WalEntry {
    /// A new document was inserted.
    Insert(Document),
    /// An existing document was replaced.
    Update(Document),
    /// A document was deleted.
    Delete(DocumentId),
    /// Application-level commit marker.
    Commit {
        /// Sequence number being committed.
        sequence: u64,
    },
    /// Written by [`WriteAheadLog::checkpoint`]. Everything before it is assumed
    /// to be durable in segments, so the log restarts from this record.
    Checkpoint {
        /// Last sequence number covered by the checkpoint.
        sequence: u64,
    },
}

/// Append-only, checksummed, fsynced log of [`WalEntry`] values.
#[derive(Debug)]
pub struct WriteAheadLog {
    path: PathBuf,
    file: File,
    last_sequence: u64,
}

impl WriteAheadLog {
    /// Opens the log at `path`, creating it if missing and truncating any torn tail.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let last_sequence = if path.exists() {
            let (records, valid_len) = scan(path)?;
            let file_len = fs::metadata(path)?.len();
            if valid_len < file_len {
                let file = OpenOptions::new().write(true).open(path)?;
                file.set_len(valid_len)?;
                file.sync_all()?;
            }
            records.last().map_or(0, |(seq, _)| *seq)
        } else {
            0
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(WriteAheadLog {
            path: path.to_path_buf(),
            file,
            last_sequence,
        })
    }

    /// Sequence number of the most recent record (0 if the log is empty).
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// Appends an entry, fsyncs it, and returns its sequence number.
    pub fn append(&mut self, entry: WalEntry) -> Result<u64, StorageError> {
        let sequence = self.last_sequence + 1;
        let payload = postcard::to_stdvec(&(sequence, &entry))
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let len = u32::try_from(payload.len())
            .map_err(|_| StorageError::Serialization("WAL record exceeds 4 GiB".into()))?;
        let mut record = Vec::with_capacity(HEADER_LEN + payload.len());
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        record.extend_from_slice(&payload);

        self.file.write_all(&record)?;
        self.file.sync_data()?;
        self.last_sequence = sequence;
        Ok(sequence)
    }

    /// Returns every valid record in order, as `(sequence, entry)` pairs.
    pub fn replay(&self) -> Result<Vec<(u64, WalEntry)>, StorageError> {
        Ok(scan(&self.path)?.0)
    }

    /// Replaces the log with a single [`WalEntry::Checkpoint`] record.
    ///
    /// The caller must have made all prior entries durable elsewhere (for
    /// example by flushing segments) before calling this. The swap is atomic:
    /// the new log is written and fsynced to a temporary file, then renamed over
    /// the old one.
    pub fn checkpoint(&mut self) -> Result<(), StorageError> {
        let tmp = self.path.with_extension("log.tmp");
        {
            let mut out = File::create(&tmp)?;
            let entry = WalEntry::Checkpoint {
                sequence: self.last_sequence,
            };
            let payload = postcard::to_stdvec(&(self.last_sequence, &entry))
                .map_err(|e| StorageError::Serialization(e.to_string()))?;
            let len = payload.len() as u32;
            out.write_all(&len.to_le_bytes())?;
            out.write_all(&crc32fast::hash(&payload).to_le_bytes())?;
            out.write_all(&payload)?;
            out.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        self.file = OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }

    /// Path of the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Reads all valid records. Returns them with the byte length of the valid prefix.
fn scan(path: &Path) -> Result<(Vec<(u64, WalEntry)>, u64), StorageError> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;

    let mut records = Vec::new();
    let mut offset = 0usize;
    let mut last_seq = 0u64;
    while bytes.len() - offset >= HEADER_LEN {
        let len =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
        let crc = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("4 bytes"));
        let start = offset + HEADER_LEN;
        let Some(end) = start.checked_add(len).filter(|e| *e <= bytes.len()) else {
            break; // torn tail: payload incomplete
        };
        let payload = &bytes[start..end];
        if crc32fast::hash(payload) != crc {
            break; // torn or corrupt record
        }
        let Ok((seq, entry)) = postcard::from_bytes::<(u64, WalEntry)>(payload) else {
            break;
        };
        // A log written after a checkpoint starts at the checkpoint's sequence, so the
        // first record may start later than 1 only if it is that checkpoint.
        let first = records.is_empty();
        let starts_ok = if first {
            seq == 1 || matches!(entry, WalEntry::Checkpoint { .. })
        } else {
            seq == last_seq + 1
        };
        if !starts_ok {
            return Err(StorageError::Corrupt(format!(
                "WAL sequence gap: expected {}, found {seq}",
                last_seq + 1
            )));
        }
        last_seq = seq;
        records.push((seq, entry));
        offset = end;
    }
    Ok((records, offset as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tpt-wal-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("wal.log")
    }

    #[test]
    fn append_then_reopen_replays_in_order() {
        let path = temp_path("reopen");
        {
            let mut wal = WriteAheadLog::open(&path).unwrap();
            assert_eq!(wal.append(WalEntry::Delete("a".into())).unwrap(), 1);
            assert_eq!(wal.append(WalEntry::Commit { sequence: 1 }).unwrap(), 2);
        }
        let wal = WriteAheadLog::open(&path).unwrap();
        assert_eq!(wal.last_sequence(), 2);
        let records = wal.replay().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0], (1, WalEntry::Delete("a".into())));
    }

    #[test]
    fn torn_tail_is_truncated_on_open() {
        let path = temp_path("torn");
        {
            let mut wal = WriteAheadLog::open(&path).unwrap();
            wal.append(WalEntry::Delete("a".into())).unwrap();
        }
        let good_len = fs::metadata(&path).unwrap().len();
        // Simulate a crash mid-write: a header promising more bytes than exist.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[99, 0, 0, 0, 1, 2, 3, 4, 5]).unwrap();
        drop(f);

        let mut wal = WriteAheadLog::open(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), good_len);
        assert_eq!(wal.replay().unwrap().len(), 1);
        assert_eq!(wal.append(WalEntry::Delete("b".into())).unwrap(), 2);
    }

    #[test]
    fn corrupted_record_stops_replay() {
        let path = temp_path("crc");
        {
            let mut wal = WriteAheadLog::open(&path).unwrap();
            wal.append(WalEntry::Delete("a".into())).unwrap();
            wal.append(WalEntry::Delete("b".into())).unwrap();
        }
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, bytes).unwrap();
        let wal = WriteAheadLog::open(&path).unwrap();
        assert_eq!(wal.last_sequence(), 1);
    }

    #[test]
    fn checkpoint_keeps_sequence_and_shrinks_log() {
        let path = temp_path("checkpoint");
        let mut wal = WriteAheadLog::open(&path).unwrap();
        for i in 0..50 {
            wal.append(WalEntry::Delete(format!("d{i}").into()))
                .unwrap();
        }
        let before = fs::metadata(&path).unwrap().len();
        wal.checkpoint().unwrap();
        assert!(fs::metadata(&path).unwrap().len() < before);
        assert_eq!(wal.append(WalEntry::Commit { sequence: 51 }).unwrap(), 51);

        let reopened = WriteAheadLog::open(&path).unwrap();
        assert_eq!(reopened.last_sequence(), 51);
        let seqs: Vec<u64> = reopened
            .replay()
            .unwrap()
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        assert_eq!(seqs, [50, 51]);
    }
}

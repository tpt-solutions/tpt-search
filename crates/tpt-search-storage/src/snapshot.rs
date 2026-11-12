//! Point-in-time snapshots and restores of a storage directory.
//!
//! A snapshot is a copy of every regular file in the source directory, plus a
//! `MANIFEST.json` recording each file's size and CRC-32. Call
//! [`SegmentManager::flush`](crate::SegmentManager::flush) and, if needed,
//! [`WriteAheadLog::checkpoint`](crate::WriteAheadLog::checkpoint) before taking a
//! snapshot so that it reflects a consistent state.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tpt_search_core::StorageError;

/// Name of the manifest file inside a snapshot directory.
pub const MANIFEST_NAME: &str = "MANIFEST.json";

/// Per-file record stored in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// File size in bytes.
    pub size: u64,
    /// CRC-32 of the file contents.
    pub crc32: u32,
}

/// Contents of `MANIFEST.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version.
    pub version: u32,
    /// Files in the snapshot, keyed by file name.
    pub files: BTreeMap<String, ManifestEntry>,
}

/// Copies every regular file in `src` into `dst` and writes a manifest.
///
/// `dst` is created if needed and must not already contain a manifest.
pub fn create_snapshot(src: &Path, dst: &Path) -> Result<Manifest, StorageError> {
    if dst.join(MANIFEST_NAME).exists() {
        return Err(StorageError::Corrupt(format!(
            "{} already contains a snapshot",
            dst.display()
        )));
    }
    fs::create_dir_all(dst)?;

    let mut files = BTreeMap::new();
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == MANIFEST_NAME || name.ends_with(".tmp") {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        fs::copy(&from, &to)?;
        let bytes = fs::read(&to)?;
        files.insert(
            name,
            ManifestEntry {
                size: bytes.len() as u64,
                crc32: crc32fast::hash(&bytes),
            },
        );
    }

    let manifest = Manifest { version: 1, files };
    let json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| StorageError::Serialization(e.to_string()))?;
    fs::write(dst.join(MANIFEST_NAME), json)?;
    Ok(manifest)
}

/// Checks that every file listed in the snapshot's manifest exists and matches
/// its recorded size and checksum. Extra files are ignored.
pub fn verify_snapshot(dir: &Path) -> Result<Manifest, StorageError> {
    let manifest = read_manifest(dir)?;
    for (name, expected) in &manifest.files {
        let bytes =
            fs::read(dir.join(name)).map_err(|e| StorageError::Corrupt(format!("{name}: {e}")))?;
        if bytes.len() as u64 != expected.size || crc32fast::hash(&bytes) != expected.crc32 {
            return Err(StorageError::Corrupt(format!("{name}: checksum mismatch")));
        }
    }
    Ok(manifest)
}

/// Verifies a snapshot and copies its files into `dst`.
pub fn restore_snapshot(snapshot: &Path, dst: &Path) -> Result<Manifest, StorageError> {
    let manifest = verify_snapshot(snapshot)?;
    fs::create_dir_all(dst)?;
    for name in manifest.files.keys() {
        fs::copy(snapshot.join(name), dst.join(name))?;
    }
    Ok(manifest)
}

fn read_manifest(dir: &Path) -> Result<Manifest, StorageError> {
    let bytes = fs::read(dir.join(MANIFEST_NAME))?;
    serde_json::from_slice(&bytes).map_err(|e| StorageError::Serialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_verifies_and_detects_tampering() {
        let base = std::env::temp_dir().join(format!("tpt-snap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let src = base.join("src");
        let snap = base.join("snap");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("wal.log"), b"abc").unwrap();
        fs::write(src.join("seg-0001.tseg"), b"segment bytes").unwrap();

        let manifest = create_snapshot(&src, &snap).unwrap();
        assert_eq!(manifest.files.len(), 2);
        verify_snapshot(&snap).unwrap();

        fs::write(snap.join("wal.log"), b"abd").unwrap();
        assert!(verify_snapshot(&snap).is_err());

        let restored = base.join("restored");
        fs::write(snap.join("wal.log"), b"abc").unwrap();
        restore_snapshot(&snap, &restored).unwrap();
        assert_eq!(fs::read(restored.join("wal.log")).unwrap(), b"abc");
    }
}

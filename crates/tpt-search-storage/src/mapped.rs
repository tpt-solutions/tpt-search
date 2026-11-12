//! Memory-mapped, checksummed segment files.
//!
//! File layout (all integers little-endian):
//!
//! ```text
//! magic[8]          b"TPTSEG\x00\x01"
//! meta_len: u32     length of the metadata block
//! meta[meta_len]    postcard(SegmentMetadata)
//! payload_len: u64  length of the payload block
//! crc32: u32        CRC-32 of the payload
//! payload[..]       postcard(Segment)
//! ```
//!
//! The metadata is readable without decoding the payload. The payload is only
//! decoded by [`MappedSegment::load`], after the checksum has been verified.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use memmap2::Mmap;
use serde::{Deserialize, Serialize};
use tpt_search_core::StorageError;

use crate::segment::{Segment, SegmentId};

const MAGIC: &[u8; 8] = b"TPTSEG\x00\x01";

/// Cheap-to-read summary stored ahead of the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentMetadata {
    /// Segment identifier.
    pub id: SegmentId,
    /// Live document count at the time the file was written.
    pub live_docs: u64,
}

/// A segment file opened through a read-only memory map.
#[derive(Debug)]
pub struct MappedSegment {
    mmap: Mmap,
    metadata: SegmentMetadata,
    payload_start: usize,
    payload_len: usize,
}

impl MappedSegment {
    /// Maps a segment file and verifies its header and payload checksum.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let file = File::open(path)?;
        // SAFETY: the map is read-only and this process never writes segment files
        // while they are mapped. Segments are replaced by writing a new file and
        // renaming it into place (see `Segment::write_to`), not by editing in place.
        // Another process truncating the file while it is mapped is outside what
        // this crate guards against.
        let mmap = unsafe { Mmap::map(&file)? };
        let bytes: &[u8] = &mmap;

        if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
            return Err(StorageError::Corrupt(format!(
                "{}: bad magic",
                path.display()
            )));
        }
        let mut pos = MAGIC.len();
        let meta_len = read_u32(bytes, &mut pos, path)? as usize;
        let meta_bytes = take(bytes, &mut pos, meta_len, path)?;
        let metadata: SegmentMetadata = postcard::from_bytes(meta_bytes)
            .map_err(|e| StorageError::Corrupt(format!("{}: metadata: {e}", path.display())))?;

        let payload_len = read_u64(bytes, &mut pos, path)? as usize;
        let expected_crc = read_u32(bytes, &mut pos, path)?;
        let payload_start = pos;
        let payload = take(bytes, &mut pos, payload_len, path)?;
        if crc32fast::hash(payload) != expected_crc {
            return Err(StorageError::Corrupt(format!(
                "{}: checksum mismatch",
                path.display()
            )));
        }

        Ok(MappedSegment {
            mmap,
            metadata,
            payload_start,
            payload_len,
        })
    }

    /// Header metadata.
    pub fn metadata(&self) -> &SegmentMetadata {
        &self.metadata
    }

    /// The verified, still-encoded payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.mmap[self.payload_start..self.payload_start + self.payload_len]
    }

    /// Decodes the payload into a [`Segment`].
    pub fn load(&self) -> Result<Segment, StorageError> {
        postcard::from_bytes(self.payload())
            .map_err(|e| StorageError::Serialization(format!("segment payload: {e}")))
    }
}

impl Segment {
    /// Writes the segment to `path` atomically: the bytes go to a temporary file,
    /// which is fsynced and then renamed over `path`.
    pub fn write_to(&self, path: &Path) -> Result<(), StorageError> {
        let metadata = SegmentMetadata {
            id: self.id,
            live_docs: self.live_count() as u64,
        };
        let meta = postcard::to_stdvec(&metadata)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        let payload =
            postcard::to_stdvec(self).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let tmp = path.with_extension("tseg.tmp");
        {
            let mut out = File::create(&tmp)?;
            out.write_all(MAGIC)?;
            out.write_all(&(meta.len() as u32).to_le_bytes())?;
            out.write_all(&meta)?;
            out.write_all(&(payload.len() as u64).to_le_bytes())?;
            out.write_all(&crc32fast::hash(&payload).to_le_bytes())?;
            out.write_all(&payload)?;
            out.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn take<'a>(
    bytes: &'a [u8],
    pos: &mut usize,
    len: usize,
    path: &Path,
) -> Result<&'a [u8], StorageError> {
    let end = pos
        .checked_add(len)
        .filter(|e| *e <= bytes.len())
        .ok_or_else(|| StorageError::Corrupt(format!("{}: truncated", path.display())))?;
    let slice = &bytes[*pos..end];
    *pos = end;
    Ok(slice)
}

fn read_u32(bytes: &[u8], pos: &mut usize, path: &Path) -> Result<u32, StorageError> {
    let raw = take(bytes, pos, 4, path)?;
    Ok(u32::from_le_bytes(raw.try_into().expect("4 bytes")))
}

fn read_u64(bytes: &[u8], pos: &mut usize, path: &Path) -> Result<u64, StorageError> {
    let raw = take(bytes, pos, 8, path)?;
    Ok(u64::from_le_bytes(raw.try_into().expect("8 bytes")))
}

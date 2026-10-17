//! File based backend.
//!
//! Reads and writes records in length-prefixed format.

use crate::backend::{ByteStore, StoredRecord, record_size};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::{
    fs::File,
    io,
    sync::atomic::{AtomicU64, Ordering},
};

pub struct FileByteStore {
    file: File,
    end: AtomicU64,
}

impl FileByteStore {
    fn open(path: &str) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;
        let end = AtomicU64::new(file.metadata()?.len());
        Ok(Self { file, end })
    }
}

impl ByteStore for FileByteStore {
    fn read_record(&self, offset: u64) -> std::io::Result<Vec<u8>> {
        let mut len_bytes = [0u8; 4];
        self.file.read_exact_at(&mut len_bytes, offset)?;
        let len = u32::from_le_bytes(len_bytes) as usize;

        let mut data = vec![0u8; len];
        self.file.read_exact_at(&mut data, offset + 4)?;
        Ok(data)
    }

    /// Writes one record at `offset`. Returns the offset just past it.
    fn write_record_at(&self, offset: u64, data: &[u8]) -> std::io::Result<u64> {
        let len = u32::try_from(data.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "record too large")
        })?;

        // Single buffer so the prefix and payload go out in one write
        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(data);

        self.file.write_all_at(&buf, offset)?;
        Ok(offset + buf.len() as u64)
    }

    /// Appends a record and returns its starting offset.
    fn append(&self, data: &[u8]) -> std::io::Result<u64> {
        let len = u32::try_from(data.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "record too large")
        })?;

        let mut buf = Vec::with_capacity(4 + data.len());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(data);

        // Atomically reserve a region, so concurrent appenders never overlap
        let offset = self.end.fetch_add(buf.len() as u64, Ordering::SeqCst);
        self.file.write_all_at(&buf, offset)?;
        Ok(offset)
    }

    /// Walks the file from the start, using each length prefix to find the next record.
    ///
    /// Only records that ended before the scan started are visited. An append racing with the
    /// scan may have reserved its region without writing it yet, so it can show up as an empty
    /// or failed record.
    fn records(&self) -> impl Iterator<Item = io::Result<StoredRecord>> {
        FileRecords {
            store: self,
            next_offset: 0,
            end: self.end.load(Ordering::SeqCst),
        }
    }
}

struct FileRecords<'a> {
    store: &'a FileByteStore,
    next_offset: u64,
    end: u64,
}

impl FileRecords<'_> {
    /// Reads the record at `offset` and returns it with the offset just past it.
    fn read_at(&self, offset: u64) -> io::Result<(StoredRecord, u64)> {
        let payload = self.store.read_record(offset)?;
        let next_offset = offset
            .checked_add(record_size(payload.len())?)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record ends past the last addressable offset",
                )
            })?;
        Ok((StoredRecord { offset, payload }, next_offset))
    }
}

impl Iterator for FileRecords<'_> {
    type Item = io::Result<StoredRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_offset >= self.end {
            return None;
        }

        match self.read_at(self.next_offset) {
            Ok((record, next_offset)) => {
                self.next_offset = next_offset;
                Some(Ok(record))
            },
            Err(error) => {
                // Without a readable length prefix there is no way to find the next record, so
                // the first failure ends the scan.
                self.next_offset = self.end;
                Some(Err(error))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use rstest::{fixture, rstest};

    use super::*;

    /// A file path unique to one test, removed when the test ends.
    struct TempFile(PathBuf);

    impl TempFile {
        fn open_store(&self) -> FileByteStore {
            FileByteStore::open(self.0.to_str().unwrap()).unwrap()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            // A failed removal only leaves a stray file in the temp directory.
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[fixture]
    fn temp_file() -> TempFile {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        TempFile(std::env::temp_dir().join(format!(
            "db-data-structures-{}-{id}.log",
            std::process::id()
        )))
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::one(&[&b"only"[..]])]
    #[case::with_empty_record(&[&b"first"[..], b"", b"third record"])]
    fn records_yields_every_appended_record_in_order(
        temp_file: TempFile,
        #[case] payloads: &[&[u8]],
    ) {
        let store = temp_file.open_store();
        let offsets: Vec<u64> = payloads.iter().map(|p| store.append(p).unwrap()).collect();

        let records: Vec<StoredRecord> = store.records().map(Result::unwrap).collect();

        let expected: Vec<StoredRecord> = offsets
            .into_iter()
            .zip(payloads)
            .map(|(offset, payload)| StoredRecord {
                offset,
                payload: payload.to_vec(),
            })
            .collect();
        assert_eq!(records, expected);
    }

    #[rstest]
    fn records_survive_reopening_the_file(temp_file: TempFile) {
        temp_file.open_store().append(b"persisted").unwrap();

        let records: Vec<StoredRecord> = temp_file
            .open_store()
            .records()
            .map(Result::unwrap)
            .collect();

        assert_eq!(
            records,
            [StoredRecord {
                offset: 0,
                payload: b"persisted".to_vec()
            }]
        );
    }

    #[rstest]
    fn records_ends_after_a_truncated_record(temp_file: TempFile) {
        let store = temp_file.open_store();
        store.append(b"whole").unwrap();
        store.append(b"torn").unwrap();
        let len = store.file.metadata().unwrap().len();
        store.file.set_len(len.saturating_sub(2)).unwrap();

        let mut records = temp_file
            .open_store()
            .records()
            .collect::<Vec<_>>()
            .into_iter();

        assert_eq!(records.next().unwrap().unwrap().payload, b"whole");
        assert_eq!(
            records.next().unwrap().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(records.next().is_none());
    }
}

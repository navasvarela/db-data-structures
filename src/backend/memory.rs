//! Memory based backend.
//!
//! Stores each record as a whole, immutable allocation in a lock-free ordered map keyed by its
//! byte offset.

use std::{
    io,
    sync::atomic::{AtomicU64, Ordering},
};

use crossbeam_skiplist::SkipMap;

use crate::backend::{ByteStore, StoredRecord, record_size};

/// A [`ByteStore`] that keeps records in memory and never takes a lock.
///
/// Length prefixes are not stored, but offsets still reserve room for them so that this backend
/// hands out the same offsets as a [`FileByteStore`](super::file::FileByteStore).
///
/// Records are stored whole rather than as bytes in a shared buffer, so readers can never see a
/// record that is half old and half new while a writer overwrites it.
///
/// Unlike a file, a record written at an offset inside another record does not overwrite part
/// of it: it becomes a separate record that only `read_record` at that exact offset returns.
#[derive(Debug, Default)]
pub struct MemoryByteStore {
    records: SkipMap<u64, Box<[u8]>>,
    end: AtomicU64,
}

impl MemoryByteStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves `end` forward so later appends never reserve space that `write_record_at` used.
    fn extend_end_to(&self, offset: u64) {
        self.end.fetch_max(offset, Ordering::Relaxed);
    }
}

impl ByteStore for MemoryByteStore {
    fn read_record(&self, offset: u64) -> io::Result<Vec<u8>> {
        self.records
            .get(&offset)
            .map(|entry| entry.value().to_vec())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no record starts at offset {offset}"),
                )
            })
    }

    /// Writes one record at `offset`, replacing any record that starts there. Returns the
    /// offset just past it.
    fn write_record_at(&self, offset: u64, data: &[u8]) -> io::Result<u64> {
        let next = offset
            .checked_add(record_size(data.len())?)
            .ok_or_else(offset_overflow)?;

        self.records.insert(offset, data.into());
        self.extend_end_to(next);
        Ok(next)
    }

    /// Appends a record and returns its starting offset.
    fn append(&self, data: &[u8]) -> io::Result<u64> {
        let size = record_size(data.len())?;

        // `end` only has to hand out non-overlapping regions, and every read-modify-write on a
        // single atomic sees the latest value regardless of ordering, so `Relaxed` is enough.
        // Making the record itself visible to other threads is the skip map's job.
        let offset = self
            .end
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |end| {
                end.checked_add(size)
            })
            .map_err(|_| offset_overflow())?;

        self.records.insert(offset, data.into());
        Ok(offset)
    }

    /// Records inserted while the iterator is running may or may not be yielded, depending on
    /// whether the iterator has already passed their offset.
    fn records(&self) -> impl Iterator<Item = io::Result<StoredRecord>> {
        self.records.iter().map(|entry| {
            Ok(StoredRecord {
                offset: *entry.key(),
                payload: entry.value().to_vec(),
            })
        })
    }
}

fn offset_overflow() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "record would end past the last addressable offset",
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc, thread};

    use rstest::{fixture, rstest};

    use super::*;

    type Write = fn(&MemoryByteStore, &[u8]) -> io::Result<u64>;

    #[fixture]
    fn backend() -> MemoryByteStore {
        MemoryByteStore::new()
    }

    #[rstest]
    fn appended_records_read_back_at_their_offsets(backend: MemoryByteStore) {
        let records: [&[u8]; 3] = [b"first", b"", b"third record"];

        let offsets: Vec<u64> = records.iter().map(|r| backend.append(r).unwrap()).collect();

        assert_eq!(offsets, [0, 9, 13]);
        for (offset, record) in offsets.iter().zip(records) {
            assert_eq!(backend.read_record(*offset).unwrap(), record);
        }
    }

    #[rstest]
    fn write_record_at_replaces_the_record_and_returns_the_next_offset(backend: MemoryByteStore) {
        let offset = backend.append(b"old").unwrap();

        let next = backend.write_record_at(offset, b"new value").unwrap();

        assert_eq!(next, 13);
        assert_eq!(backend.read_record(offset).unwrap(), b"new value");
    }

    #[rstest]
    fn append_after_write_record_at_does_not_reuse_its_space(backend: MemoryByteStore) {
        let next = backend.write_record_at(100, b"far").unwrap();

        assert_eq!(backend.append(b"after").unwrap(), next);
        assert_eq!(backend.read_record(100).unwrap(), b"far");
    }

    #[rstest]
    fn read_record_at_an_offset_without_a_record_is_not_found(
        backend: MemoryByteStore,
        #[values(1, 10, u64::MAX)] offset: u64,
    ) {
        backend.append(b"record").unwrap();

        let error = backend.read_record(offset).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[rstest]
    #[case::append(|backend: &MemoryByteStore, data: &[u8]| backend.append(data))]
    #[case::write_record_at(|backend: &MemoryByteStore, data: &[u8]| {
        backend.write_record_at(u64::MAX - 4, data)
    })]
    fn writes_past_the_last_addressable_offset_fail_without_storing(#[case] write: Write) {
        let backend = MemoryByteStore {
            end: AtomicU64::new(u64::MAX - 4),
            ..Default::default()
        };

        let error = write(&backend, b"x").unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(backend.records.is_empty());
        assert_eq!(backend.end.load(Ordering::Relaxed), u64::MAX - 4);
    }

    #[rstest]
    fn records_yields_every_record_in_offset_order(backend: MemoryByteStore) {
        backend.write_record_at(100, b"far").unwrap();
        backend.append(b"after").unwrap();
        backend.write_record_at(0, b"first").unwrap();

        let records: Vec<StoredRecord> = backend.records().map(Result::unwrap).collect();

        let expected = [(0, &b"first"[..]), (100, b"far"), (107, b"after")];
        let actual: Vec<(u64, &[u8])> = records
            .iter()
            .map(|r| (r.offset, r.payload.as_slice()))
            .collect();
        assert_eq!(actual, expected);
    }

    #[rstest]
    fn records_of_an_empty_store_is_empty(backend: MemoryByteStore) {
        assert_eq!(backend.records().count(), 0);
    }

    #[test]
    fn concurrent_appends_get_distinct_offsets_and_keep_their_data() {
        const THREADS: u64 = 8;
        const APPENDS_PER_THREAD: u64 = 1_000;
        let backend = Arc::new(MemoryByteStore::new());

        let handles: Vec<_> = (0..THREADS)
            .map(|thread_id| {
                let backend = Arc::clone(&backend);
                thread::spawn(move || {
                    (0..APPENDS_PER_THREAD)
                        .map(|i| {
                            let payload = format!("{thread_id}:{i}").into_bytes();
                            (backend.append(&payload).unwrap(), payload)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let written: Vec<(u64, Vec<u8>)> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();

        let offsets: HashSet<u64> = written.iter().map(|(offset, _)| *offset).collect();
        assert_eq!(offsets.len(), written.len());
        for (offset, payload) in &written {
            assert_eq!(&backend.read_record(*offset).unwrap(), payload);
        }
        let total_size: u64 = written
            .iter()
            .map(|(_, p)| record_size(p.len()).unwrap())
            .sum();
        assert_eq!(backend.end.load(Ordering::Relaxed), total_size);
    }
}

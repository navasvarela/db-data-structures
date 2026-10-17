//! File based backend.
//!
//! Reads and writes records in length-prefixed format.

#[cfg(unix)]
use std::os::unix::fs::FileExt;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::backend::{
    ByteStore, CompactionStores, LENGTH_PREFIX_LEN, StoredRecord, offset_overflow, record_size,
};

pub struct FileByteStore {
    file: File,
    end: AtomicU64,
}

impl FileByteStore {
    /// Opens the store at `path`, creating an empty one if the file does not exist yet.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Self::from_file(file)
    }

    /// Creates an empty store at `path`, discarding any file already there.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        Self::from_file(file)
    }

    fn from_file(file: File) -> io::Result<Self> {
        let end = AtomicU64::new(file.metadata()?.len());
        Ok(Self { file, end })
    }
}

impl ByteStore for FileByteStore {
    fn read_record(&self, offset: u64) -> io::Result<Vec<u8>> {
        let mut len_bytes = [0u8; size_of::<u32>()];
        self.file.read_exact_at(&mut len_bytes, offset)?;
        let len = usize::try_from(u32::from_le_bytes(len_bytes)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "record is longer than this platform can address",
            )
        })?;

        let mut data = vec![0u8; len];
        let payload_offset = offset
            .checked_add(LENGTH_PREFIX_LEN)
            .ok_or_else(offset_overflow)?;
        self.file.read_exact_at(&mut data, payload_offset)?;
        Ok(data)
    }

    /// Writes one record at `offset`. Returns the offset just past it.
    fn write_record_at(&self, offset: u64, data: &[u8]) -> io::Result<u64> {
        let next = offset
            .checked_add(record_size(data.len())?)
            .ok_or_else(offset_overflow)?;
        self.file.write_all_at(&length_prefixed(data)?, offset)?;
        Ok(next)
    }

    /// Appends a record and returns its starting offset.
    fn append(&self, data: &[u8]) -> io::Result<u64> {
        let size = record_size(data.len())?;
        let buf = length_prefixed(data)?;

        // Atomically reserve a region, so concurrent appenders never overlap
        let offset = self
            .end
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |end| {
                end.checked_add(size)
            })
            .map_err(|_| offset_overflow())?;
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

/// Builds the on-disk form of a record in one buffer, so the prefix and payload go out in a
/// single write.
fn length_prefixed(data: &[u8]) -> io::Result<Vec<u8>> {
    let len = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record too large"))?;

    let mut buf = Vec::with_capacity(data.len().saturating_add(size_of::<u32>()));
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(data);
    Ok(buf)
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
            .ok_or_else(offset_overflow)?;
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

/// Compacts the store at `path` by writing a new file next to it and renaming it over `path`.
///
/// The rename is atomic, so after a crash `path` holds either the old store or the complete
/// compacted one, never a mix. A crash mid-compaction leaves the half-written file at
/// [`compaction_path`](Self::compaction_path), and the next compaction overwrites it.
#[derive(Debug, Clone)]
pub struct FileCompactionStores {
    path: PathBuf,
}

impl FileCompactionStores {
    /// `path` must be the file of the store being compacted.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Where the compacted store is written before it replaces the one at `path`.
    ///
    /// The suffix is appended rather than swapped for the extension, so stores named
    /// `a.log` and `a.db` in one directory do not share a compaction file.
    pub fn compaction_path(&self) -> PathBuf {
        let mut path = OsString::from(self.path.as_os_str());
        path.push(".compacting");
        PathBuf::from(path)
    }
}

impl CompactionStores for FileCompactionStores {
    type Store = FileByteStore;

    fn create(&mut self) -> io::Result<FileByteStore> {
        FileByteStore::create(self.compaction_path())
    }

    /// Renames the compacted file over the original. The old store's open handle keeps reading
    /// the replaced file until it is dropped, so the index can keep using it if this fails.
    fn install(&mut self, compacted: &FileByteStore) -> io::Result<()> {
        // The data must be on disk before the rename makes it the store, or a crash could leave
        // `path` pointing at a file whose contents never made it out of the page cache.
        compacted.file.sync_all()?;
        fs::rename(self.compaction_path(), &self.path)?;
        sync_parent_dir(&self.path)
    }
}

/// Flushes the directory entry for `path`, so a rename into it survives a crash.
fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(parent)?.sync_all()
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
            FileByteStore::open(&self.0).unwrap()
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

    fn payloads(store: &FileByteStore) -> Vec<Vec<u8>> {
        store.records().map(|r| r.unwrap().payload).collect()
    }

    #[rstest]
    fn create_discards_the_records_already_in_the_file(temp_file: TempFile) {
        temp_file.open_store().append(b"stale").unwrap();

        let store = FileByteStore::create(&temp_file.0).unwrap();

        assert_eq!(payloads(&store), Vec::<Vec<u8>>::new());
        assert_eq!(store.append(b"fresh").unwrap(), 0);
    }

    #[rstest]
    fn install_replaces_the_original_file_with_the_compacted_one(temp_file: TempFile) {
        let old = temp_file.open_store();
        old.append(b"old").unwrap();
        let mut stores = FileCompactionStores::new(&temp_file.0);
        let compacted = stores.create().unwrap();
        compacted.append(b"compacted").unwrap();

        stores.install(&compacted).unwrap();

        assert_eq!(payloads(&temp_file.open_store()), [b"compacted"]);
        assert!(!stores.compaction_path().exists());
        assert_eq!(
            payloads(&old),
            [b"old"],
            "the old handle still reads the replaced file"
        );
    }

    #[rstest]
    fn failed_install_leaves_the_original_file_in_place(temp_file: TempFile) {
        temp_file.open_store().append(b"original").unwrap();
        let mut stores = FileCompactionStores::new(&temp_file.0);
        let compacted = stores.create().unwrap();
        fs::remove_file(stores.compaction_path()).unwrap();

        let error = stores.install(&compacted).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(payloads(&temp_file.open_store()), [b"original"]);
    }

    #[test]
    fn compaction_path_appends_a_suffix_instead_of_replacing_the_extension() {
        let stores = FileCompactionStores::new("dir/store.log");

        assert_eq!(
            stores.compaction_path(),
            Path::new("dir/store.log.compacting")
        );
    }
}

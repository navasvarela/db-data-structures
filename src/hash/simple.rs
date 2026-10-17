use std::collections::HashMap;

use tracing::warn;

use crate::{
    backend::{ByteStore, CompactionStores, record_size},
    hash::{HashIndex, HashIndexError, record},
};

/// A hash index over an append-only log of key-value records.
///
/// Every `set` appends a new record and repoints the key at it, so a value being written never
/// shares bytes with one already stored: if the append fails, the key keeps its previous value.
/// Overwritten records stay behind as dead bytes. Once they outweigh the live ones, the index
/// compacts itself by copying the live records into an empty store from its
/// [`CompactionStores`], which then puts that store in place of the old one.
pub struct SimpleHashIndex<C: CompactionStores> {
    store: C::Store,
    keydir: KeyDir,
    compaction_stores: C,
}

impl<C: CompactionStores> SimpleHashIndex<C> {
    /// Opens an index over `store`, rebuilding it from the records already there.
    ///
    /// `compaction_stores` must replace `store` itself when it installs a compacted store, for
    /// example by writing to the same file path.
    ///
    /// # Errors
    ///
    /// Returns an error if a record in `store` cannot be read or is not a key-value record.
    pub fn open(store: C::Store, compaction_stores: C) -> Result<Self, HashIndexError> {
        let keydir = KeyDir::rebuild_from(&store)?;
        Ok(Self {
            store,
            keydir,
            compaction_stores,
        })
    }

    /// Copies every live record into a new store, installs it in place of the current one and
    /// switches the index over to it, dropping the old store along with all its dead records.
    ///
    /// # Errors
    ///
    /// Returns the first error from creating the new store, reading the old one, copying a
    /// record or installing the new store. The index is then left unchanged, still reading from
    /// the old store.
    pub fn compact(&mut self) -> Result<(), HashIndexError> {
        let fresh = self.compaction_stores.create()?;
        let mut keydir = KeyDir::default();
        for (key, location) in self.keydir.iter() {
            let payload = self.store.read_record(location.offset)?;
            let offset = fresh.append(&payload)?;
            keydir.insert(
                key.to_owned(),
                RecordLocation {
                    offset,
                    size: location.size,
                },
            );
        }

        self.compaction_stores.install(&fresh)?;
        self.store = fresh;
        self.keydir = keydir;
        Ok(())
    }

    /// Compacts once dead bytes outweigh live ones, without failing the write that triggered it.
    ///
    /// That write is already committed, so reporting a compaction error from `set` would make
    /// the caller believe the value was not stored. A failed compaction is logged instead, and
    /// is retried after the next write because the dead bytes are still over the threshold.
    fn compact_if_needed(&mut self) {
        if !self.keydir.needs_compaction() {
            return;
        }
        if let Err(error) = self.compact() {
            warn!("Compaction failed, retrying after the next write: {error}");
        }
    }

    fn read_value(&self, offset: u64) -> Result<String, HashIndexError> {
        let payload = self.store.read_record(offset)?;
        let record = record::decode(&payload)
            .map_err(|source| HashIndexError::CorruptRecord { offset, source })?;
        Ok(String::from_utf8_lossy(record.value).into_owned())
    }
}

impl<C: CompactionStores> HashIndex for SimpleHashIndex<C> {
    fn get(&self, key: String) -> Option<String> {
        let location = self.keydir.get(&key)?;
        self.read_value(location.offset)
            .inspect_err(|e| warn!("While reading record: {e}"))
            .ok()
    }

    fn set(&mut self, key: String, value: String) -> Result<(), HashIndexError> {
        let payload = record::encode(&key, value.as_bytes())?;
        let size = record_size(payload.len())?;
        // The key is only repointed once the append has fully succeeded, which makes updating
        // `keydir` the commit point: a failed append leaves the key on its previous, untouched
        // record.
        let offset = self.store.append(&payload)?;
        self.keydir.insert(key, RecordLocation { offset, size });

        self.compact_if_needed();
        Ok(())
    }
}

/// Where a key's current record sits in the store and how much space it takes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecordLocation {
    offset: u64,
    /// Includes the length prefix, so dead and live byte counts match the store's real usage.
    size: u64,
}

/// The in-memory map from each key to its latest record, plus how much of the store those
/// records (live) and the ones they replaced (dead) take up.
#[derive(Debug, Default)]
struct KeyDir {
    locations: HashMap<String, RecordLocation>,
    live_bytes: u64,
    dead_bytes: u64,
}

impl KeyDir {
    /// Replays every record in `store`, as if each had just been written by `set`.
    fn rebuild_from(store: &impl ByteStore) -> Result<Self, HashIndexError> {
        let mut keydir = Self::default();
        // `records` yields records in ascending offset order, which is the order they were
        // appended in, so a key's later record replaces its earlier ones here as it did then.
        for stored in store.records() {
            let stored = stored?;
            let record = record::decode(&stored.payload).map_err(|source| {
                HashIndexError::CorruptRecord {
                    offset: stored.offset,
                    source,
                }
            })?;
            let location = RecordLocation {
                offset: stored.offset,
                size: record_size(stored.payload.len())?,
            };
            keydir.insert(record.key.to_owned(), location);
        }
        Ok(keydir)
    }

    fn get(&self, key: &str) -> Option<RecordLocation> {
        self.locations.get(key).copied()
    }

    fn iter(&self) -> impl Iterator<Item = (&str, RecordLocation)> {
        self.locations
            .iter()
            .map(|(key, location)| (key.as_str(), *location))
    }

    /// Points `key` at `location`, counting the record it replaces, if any, as dead.
    fn insert(&mut self, key: String, location: RecordLocation) {
        self.live_bytes = self.live_bytes.saturating_add(location.size);
        if let Some(replaced) = self.locations.insert(key, location) {
            self.live_bytes = self.live_bytes.saturating_sub(replaced.size);
            self.dead_bytes = self.dead_bytes.saturating_add(replaced.size);
        }
    }

    /// Whether overwritten records now take more space than live ones.
    ///
    /// Compacting at this point keeps the store under twice the size of its live data, and
    /// every compaction at least halves it. Each compaction copies the live bytes once, and
    /// happens only after at least that many bytes were overwritten, so its cost is amortised
    /// over those writes. A lower threshold would use less space, but would rewrite the same
    /// live records more often.
    fn needs_compaction(&self) -> bool {
        self.dead_bytes > self.live_bytes
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, io};

    use rstest::rstest;

    use super::*;
    use crate::backend::{
        StoredRecord,
        memory::{MemoryByteStore, MemoryCompactionStores},
    };

    type Entries<'a> = &'a [(&'a str, &'a str)];
    type MemoryIndex = SimpleHashIndex<MemoryCompactionStores>;
    type FlakyIndex = SimpleHashIndex<FlakyStores>;

    /// A memory store whose appends can be made to fail, to exercise error paths.
    #[derive(Default)]
    struct FlakyStore {
        inner: MemoryByteStore,
        appends_fail: Cell<bool>,
    }

    impl ByteStore for FlakyStore {
        fn read_record(&self, offset: u64) -> io::Result<Vec<u8>> {
            self.inner.read_record(offset)
        }

        fn write_record_at(&self, offset: u64, data: &[u8]) -> io::Result<u64> {
            self.inner.write_record_at(offset, data)
        }

        fn append(&self, data: &[u8]) -> io::Result<u64> {
            if self.appends_fail.get() {
                return Err(io::Error::other("append failed"));
            }
            self.inner.append(data)
        }

        fn records(&self) -> impl Iterator<Item = io::Result<StoredRecord>> {
            self.inner.records()
        }
    }

    /// Which step of a compaction [`FlakyStores`] makes fail.
    #[derive(Debug, Clone, Copy)]
    enum CompactionFailure {
        Nothing,
        Create,
        Copy,
        Install,
    }

    struct FlakyStores(CompactionFailure);

    impl CompactionStores for FlakyStores {
        type Store = FlakyStore;

        fn create(&mut self) -> io::Result<FlakyStore> {
            match self.0 {
                CompactionFailure::Create => Err(io::Error::other("no space left")),
                CompactionFailure::Copy => Ok(FlakyStore {
                    appends_fail: Cell::new(true),
                    ..FlakyStore::default()
                }),
                CompactionFailure::Nothing | CompactionFailure::Install => {
                    Ok(FlakyStore::default())
                },
            }
        }

        fn install(&mut self, _compacted: &FlakyStore) -> io::Result<()> {
            match self.0 {
                CompactionFailure::Install => Err(io::Error::other("rename failed")),
                CompactionFailure::Nothing
                | CompactionFailure::Create
                | CompactionFailure::Copy => Ok(()),
            }
        }
    }

    fn set(index: &mut impl HashIndex, key: &str, value: &str) {
        index.set(key.to_owned(), value.to_owned()).unwrap();
    }

    fn get(index: &impl HashIndex, key: &str) -> Option<String> {
        index.get(key.to_owned())
    }

    fn index_with(entries: Entries) -> MemoryIndex {
        let mut index = MemoryIndex::open(MemoryByteStore::new(), MemoryCompactionStores).unwrap();
        for (key, value) in entries {
            set(&mut index, key, value);
        }
        index
    }

    /// Space the record for `key` and `value` takes in the store.
    fn size_of(key: &str, value: &str) -> u64 {
        record_size(record::encode(key, value.as_bytes()).unwrap().len()).unwrap()
    }

    fn usage(index: &SimpleHashIndex<impl CompactionStores>) -> (u64, u64) {
        (index.keydir.live_bytes, index.keydir.dead_bytes)
    }

    #[rstest]
    #[case::empty_index(&[], "missing", None)]
    #[case::missing_key(&[("a", "1")], "missing", None)]
    #[case::only_key(&[("a", "1")], "a", Some("1"))]
    #[case::one_of_many(&[("a", "1"), ("b", "2")], "b", Some("2"))]
    #[case::overwritten_by_longer(&[("a", "short"), ("a", "much longer")], "a", Some("much longer"))]
    #[case::overwritten_by_shorter(&[("a", "much longer"), ("a", "short")], "a", Some("short"))]
    fn get_returns_the_latest_value_set_for_each_key(
        #[case] entries: Entries,
        #[case] key: &str,
        #[case] expected: Option<&str>,
    ) {
        let index = index_with(entries);

        assert_eq!(get(&index, key).as_deref(), expected);
    }

    #[test]
    fn overwriting_a_key_counts_its_old_record_as_dead() {
        let index = index_with(&[("a", "old"), ("b", "other"), ("a", "newer")]);

        let live = size_of("b", "other") + size_of("a", "newer");
        assert_eq!(usage(&index), (live, size_of("a", "old")));
    }

    #[test]
    fn set_compacts_once_dead_bytes_exceed_live_bytes() {
        let record = size_of("a", "1");
        let mut index = index_with(&[("a", "1"), ("a", "2")]);
        assert_eq!(
            usage(&index),
            (record, record),
            "equal dead bytes do not compact yet"
        );

        set(&mut index, "a", "3");

        assert_eq!(get(&index, "a").as_deref(), Some("3"));
        assert_eq!(usage(&index), (record, 0));
        // The next free offset is the size of everything in the store, so this shows the
        // compacted store holds only the live record.
        assert_eq!(index.store.append(b"").unwrap(), record);
    }

    #[test]
    fn compact_keeps_latest_values_and_drops_dead_records() {
        let mut index = index_with(&[("a", "1"), ("b", "2"), ("a", "3"), ("c", "4")]);
        let (live_bytes, _) = usage(&index);

        index.compact().unwrap();

        for (key, value) in [("a", "3"), ("b", "2"), ("c", "4")] {
            assert_eq!(get(&index, key).as_deref(), Some(value), "key {key}");
        }
        assert_eq!(usage(&index), (live_bytes, 0));
        assert_eq!(index.store.append(b"").unwrap(), live_bytes);
    }

    #[test]
    fn failed_set_keeps_the_previous_state() {
        let mut index = FlakyIndex::open(
            FlakyStore::default(),
            FlakyStores(CompactionFailure::Nothing),
        )
        .unwrap();
        set(&mut index, "a", "old");
        index.store.appends_fail.set(true);

        let overwrite = index.set("a".to_owned(), "new".to_owned());
        let insert = index.set("b".to_owned(), "new".to_owned());

        assert!(matches!(overwrite, Err(HashIndexError::IOError(_))));
        assert!(matches!(insert, Err(HashIndexError::IOError(_))));
        assert_eq!(get(&index, "a").as_deref(), Some("old"));
        assert_eq!(get(&index, "b"), None);
        assert_eq!(usage(&index), (size_of("a", "old"), 0));
    }

    #[rstest]
    #[case::create(CompactionFailure::Create)]
    #[case::copy(CompactionFailure::Copy)]
    #[case::install(CompactionFailure::Install)]
    fn failed_compaction_keeps_the_write_and_the_old_store(#[case] failure: CompactionFailure) {
        let mut index = FlakyIndex::open(FlakyStore::default(), FlakyStores(failure)).unwrap();
        for value in ["1", "2"] {
            set(&mut index, "a", value);
        }

        let triggering_write = index.set("a".to_owned(), "3".to_owned());

        assert!(triggering_write.is_ok());
        assert_eq!(get(&index, "a").as_deref(), Some("3"));
        assert!(
            index.keydir.needs_compaction(),
            "compaction is still pending"
        );
        assert!(index.compact().is_err());
        assert_eq!(get(&index, "a").as_deref(), Some("3"));
    }

    #[rstest]
    #[case::empty(&[])]
    #[case::distinct_keys(&[("a", "1"), ("b", "2"), ("c", "3")])]
    #[case::overwrites(&[("a", "1"), ("b", "2"), ("a", "3")])]
    #[case::after_compaction(&[("a", "1"), ("a", "2"), ("a", "3"), ("b", "4"), ("b", "5")])]
    fn reopening_a_store_rebuilds_the_same_index(#[case] entries: Entries) {
        let index = index_with(entries);
        let expected_usage = usage(&index);
        let values: Vec<(&str, Option<String>)> = entries
            .iter()
            .map(|(key, _)| (*key, get(&index, key)))
            .collect();

        let SimpleHashIndex { store, .. } = index;
        let reopened = MemoryIndex::open(store, MemoryCompactionStores).unwrap();

        assert_eq!(usage(&reopened), expected_usage);
        for (key, value) in values {
            assert_eq!(get(&reopened, key), value, "key {key}");
        }
    }

    #[rstest]
    #[case::too_short_for_key_length(&[1, 0])]
    #[case::key_past_the_end(&[9, 0, 0, 0, b'a'])]
    #[case::key_not_utf8(&[1, 0, 0, 0, 0xFF])]
    fn open_rejects_records_that_are_not_key_value_records(#[case] payload: &[u8]) {
        let store = MemoryByteStore::new();
        store
            .append(&record::encode("good", b"record").unwrap())
            .unwrap();
        let corrupt_offset = store.append(payload).unwrap();

        let result = MemoryIndex::open(store, MemoryCompactionStores);

        assert!(matches!(
            result,
            Err(HashIndexError::CorruptRecord { offset, .. }) if offset == corrupt_offset
        ));
    }
}

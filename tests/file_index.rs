//! Runs `SimpleHashIndex` on files, through compaction and reopening.

// Marks the whole crate as test code, so clippy's test allowances (`allow-unwrap-in-tests` and
// friends in `clippy.toml`) also cover the helpers outside `#[test]` functions.
#![cfg(test)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use db_data_structures::{
    backend::file::{FileByteStore, FileCompactionStores},
    hash::{HashIndex, simple::SimpleHashIndex},
};
use rstest::{fixture, rstest};

type FileIndex = SimpleHashIndex<FileCompactionStores>;

/// A file path unique to one test, removed with its compaction file when the test ends.
struct TempStore {
    path: PathBuf,
}

impl TempStore {
    fn open_index(&self) -> FileIndex {
        let store = FileByteStore::open(&self.path).unwrap();
        FileIndex::open(store, FileCompactionStores::new(&self.path)).unwrap()
    }

    fn len(&self) -> u64 {
        fs::metadata(&self.path).unwrap().len()
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        // A failed removal only leaves stray files in the temp directory.
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(FileCompactionStores::new(&self.path).compaction_path());
    }
}

#[fixture]
fn temp_store() -> TempStore {
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    TempStore {
        path: std::env::temp_dir().join(format!(
            "db-data-structures-it-{}-{id}.log",
            std::process::id()
        )),
    }
}

fn set(index: &mut impl HashIndex, key: &str, value: &str) {
    index.set(key.to_owned(), value.to_owned()).unwrap();
}

fn get(index: &impl HashIndex, key: &str) -> Option<String> {
    index.get(key.to_owned())
}

#[rstest]
fn compaction_shrinks_the_file_in_place(temp_store: TempStore) {
    let mut index = temp_store.open_index();
    set(&mut index, "key", "v1");
    let one_record = temp_store.len();
    set(&mut index, "key", "v2");

    // The third write leaves two dead records against one live one, which triggers compaction.
    set(&mut index, "key", "v3");

    assert_eq!(get(&index, "key").as_deref(), Some("v3"));
    assert_eq!(temp_store.len(), one_record);
    assert!(
        !FileCompactionStores::new(&temp_store.path)
            .compaction_path()
            .exists()
    );
}

#[rstest]
#[case::without_compaction(&[("a", "1"), ("b", "2"), ("a", "3")])]
#[case::after_compaction(&[("a", "1"), ("a", "2"), ("a", "3"), ("b", "4")])]
fn reopening_the_file_restores_every_key(temp_store: TempStore, #[case] entries: &[(&str, &str)]) {
    let mut index = temp_store.open_index();
    for (key, value) in entries {
        set(&mut index, key, value);
    }
    let expected: Vec<(&str, Option<String>)> = entries
        .iter()
        .map(|(key, _)| (*key, get(&index, key)))
        .collect();
    drop(index);

    let reopened = temp_store.open_index();

    for (key, value) in expected {
        assert_eq!(get(&reopened, key), value, "key {key}");
    }
}

#[rstest]
fn writes_after_compaction_land_in_the_installed_file(temp_store: TempStore) {
    let mut index = temp_store.open_index();
    for value in ["1", "2", "3"] {
        set(&mut index, "a", value);
    }

    set(&mut index, "b", "after");
    drop(index);

    let reopened = temp_store.open_index();
    assert_eq!(get(&reopened, "a").as_deref(), Some("3"));
    assert_eq!(get(&reopened, "b").as_deref(), Some("after"));
}

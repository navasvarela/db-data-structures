# db-data-structures

[![CI](https://github.com/navasvarela/db-data-structures/actions/workflows/ci.yml/badge.svg)](https://github.com/navasvarela/db-data-structures/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/db-data-structures.svg)](https://crates.io/crates/db-data-structures)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue)](https://navasvarela.github.io/db-data-structures/)

Data structures that databases are built from, written to be read and learned from as much as used.
The crate currently has two layers:

- **Byte stores** (`backend`): append-only storage for length-prefixed records.
  - `FileByteStore` keeps records in a single file.
  - `MemoryByteStore` keeps them in a lock-free skip map, and hands out the same offsets a file
    would for the same writes.
- **Hash index** (`hash`): `SimpleHashIndex`, a [Bitcask]-style index that keeps every key in an
  in-memory hash map pointing at its latest record in a byte store.

## How the hash index works

- **Writes only append.** `set` appends a `[key length][key][value]` record and points the key at
  it only after the append succeeds, so a failed write leaves the previous value untouched.
  Records are never overwritten in place, which would risk losing both values if a write is
  interrupted.
- **Dead records are compacted away.** The index counts how many bytes belong to overwritten
  (dead) records. Once they outweigh the live ones, it copies the live records into a fresh store
  and switches over, which keeps the store under twice the size of its live data.
- **Compaction is crash-safe on disk.** The file backend writes the compacted store next to the
  original, fsyncs it and renames it over the original. After a crash the path holds either the
  old store or the complete new one, never a mix.
- **Indexes rebuild themselves.** Opening an index replays the store's records in order, so it can
  pick up where it left off after a restart.

[Bitcask]: https://riak.com/assets/bitcask-intro.pdf

## Usage

```toml
[dependencies]
db-data-structures = "0.1"
```

An index kept in memory:

```rust
use db_data_structures::{
    backend::memory::{MemoryByteStore, MemoryCompactionStores},
    hash::{HashIndex, HashIndexError, simple::SimpleHashIndex},
};

fn main() -> Result<(), HashIndexError> {
    let mut index = SimpleHashIndex::open(MemoryByteStore::new(), MemoryCompactionStores)?;

    index.set("greeting".to_owned(), "hello".to_owned())?;
    index.set("greeting".to_owned(), "hello again".to_owned())?;

    assert_eq!(index.get("greeting".to_owned()).as_deref(), Some("hello again"));
    Ok(())
}
```

An index kept in a file, which survives restarts:

```rust,no_run
use db_data_structures::{
    backend::file::{FileByteStore, FileCompactionStores},
    hash::{HashIndex, HashIndexError, simple::SimpleHashIndex},
};

fn main() -> Result<(), HashIndexError> {
    let path = "data.log";
    let mut index = SimpleHashIndex::open(
        FileByteStore::open(path)?,
        FileCompactionStores::new(path),
    )?;

    index.set("greeting".to_owned(), "hello".to_owned())?;
    Ok(())
}
```

## Documentation

[The guide](https://navasvarela.github.io/db-data-structures/) explains each data structure
databases are built from, for both transactional and analytical systems, with the papers and books
to learn it from and the order in which this crate builds them. Its source is the mdBook in
[`book/`](https://github.com/navasvarela/db-data-structures/tree/main/book); `just book-serve`
previews it locally.

API documentation for the latest `main` is published
[alongside the guide](https://navasvarela.github.io/db-data-structures/api/db_data_structures/).
Released versions are documented on [docs.rs](https://docs.rs/db-data-structures).

## Development

The toolchain is pinned in `rust-toolchain.toml`.

```bash
cargo test
```

```bash
just lint
```

CI runs `cargo fmt --check`, clippy with warnings denied, and the tests on every pull request.
See [RELEASING.md](https://github.com/navasvarela/db-data-structures/blob/main/RELEASING.md) for how versions are published to crates.io.

## License

[MIT](https://github.com/navasvarela/db-data-structures/blob/main/LICENSE)

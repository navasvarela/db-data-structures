# db-data-structures

[![CI](https://github.com/navasvarela/db-data-structures/actions/workflows/ci.yml/badge.svg)](https://github.com/navasvarela/db-data-structures/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/db-data-structures.svg)](https://crates.io/crates/db-data-structures)
[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue)](https://navasvarela.github.io/db-data-structures/)

Data structures that databases are built from, written to be read and learned from as much as used.
The crate currently has three parts:

- **Byte stores** (`backend`): append-only storage for length-prefixed records.
  - `FileByteStore` keeps records in a single file.
  - `MemoryByteStore` keeps them in a lock-free skip map, and hands out the same offsets a file
    would for the same writes.
- **Hash index** (`hash`): `SimpleHashIndex`, a [Bitcask]-style index that keeps every key in an
  in-memory hash map pointing at its latest record in a byte store.
- **Ordered tree** (`tree`): `RedBlackTree`, an in-memory ordered map that keeps its keys sorted
  and supports range queries, the kind of structure a memtable or an ordered index is built on.

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

## How the red-black tree works

A hash index finds a key in one step but forgets the order of the keys, so it cannot answer
"every key between `a` and `f`". A binary search tree can, as long as it stays balanced: a tree
fed already sorted keys degenerates into a linked list. `RedBlackTree` is a
[left-leaning red-black tree][LLRB], which stays balanced by colouring every node red or black and
keeping these rules:

- **The root is black**, and a red node never has a red child.
- **Every path** from a node down to an empty link passes the same number of black nodes.
- **Red nodes lean left:** a red node is always a left child.

Since red nodes cannot be stacked and every path has the same number of black ones, no path is more
than twice as long as another, so the tree is at most `2·log2(n + 1)` levels deep and lookups, insertions and removals take `O(log n)` steps.

- **Insertions add a red leaf**, which never changes how many black nodes a path passes. On the
  way back up, each node on the path is repaired with at most two rotations and a colour flip,
  which push any rule violation one level up until it reaches the root.
- **Removals only unlink red nodes.** On the way down, the tree borrows a red link from a sibling
  (or merges with it) so the node it moves to is never a lone black node. The node finally
  unlinked is red, and the same repairs as insertion tidy up on the way back.
- **Nodes own their children.** Repairing on the way back out of a recursive descent, rather than
  walking up parent links as the classic red-black tree does, lets each node own its two children
  in a `Box`, so the tree is safe Rust with no reference counting or index arena.

The invariants are spelled out in the module documentation, and the tests check all of them after
every insertion and removal.

[LLRB]: https://sedgewick.io/wp-content/themes/sedgewick/papers/2008LLRB.pdf

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

An ordered tree:

```rust
use db_data_structures::tree::red_black::RedBlackTree;

let mut tree = RedBlackTree::new();
for (key, value) in [("cherry", 3), ("apple", 1), ("banana", 2), ("date", 4)] {
    tree.insert(key, value);
}

assert_eq!(tree.get(&"banana"), Some(&2));
assert_eq!(tree.min(), Some((&"apple", &1)));

let middle: Vec<_> = tree.range("b".."d").map(|(key, _)| *key).collect();
assert_eq!(middle, ["banana", "cherry"]);
```

## Documentation

API documentation for the latest `main` is published to
[GitHub Pages](https://navasvarela.github.io/db-data-structures/). Released versions are also
documented on [docs.rs](https://docs.rs/db-data-structures).

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

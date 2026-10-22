# SSTable

A sorted string table: an immutable file of key-value pairs in key order, split into blocks, with a
sparse index of the first key in each block and a footer pointing at the index.

- **Used by:** Bigtable, LevelDB, RocksDB, Cassandra, ScyllaDB.
- **Cost:** a lookup binary-searches the sparse index in memory, then reads one block.
- **Notes:** blocks usually prefix-compress keys, with periodic restart points so a block can still
  be binary-searched. A [Bloom filter](../filters/bloom-filter.md) per table avoids reading it at
  all for absent keys.

## How it works

### Layout

An SSTable is written once, front to back, from an already sorted stream of entries, and never
modified. LevelDB's format, which most engines resemble, looks like this:

<figure class="diagram">
{{#include figures/sstable-layout.svg}}
<figcaption>LevelDB's table layout. A reader starts from the footer, at a known position: it holds the handles of the index block and of the metaindex block, which in turn holds the handle of the filter block. The index has one entry per data block.</figcaption>
</figure>

A *block handle* is an `(offset, size)` pair locating a block in the file. The footer is the
only part at a known position, the end of the file, so reading always starts there. In LevelDB it
is 48 bytes: two handles, padded to 40 bytes, then the 8-byte magic number `0xdb4775248b80fb57`.
Every block on disk is followed by a 5-byte trailer: a 1-byte compression type and a 32-bit CRC
checksum.

### Inside a block

Neighbouring keys in sorted order often share a prefix, so each entry stores only what differs
from the previous key:

```text
entry:   shared_len | unshared_len | value_len | key suffix | value
         (varints)

"apple"   →  0 | 5 | ... | "apple"   | ...     ← restart point: full key
"applet"  →  5 | 1 | ... | "t"       | ...
"apply"   →  4 | 1 | ... | "y"       | ...

trailer:  restart offsets [u32; num_restarts] | num_restarts: u32
```

A key can only be rebuilt by decoding every entry since the last full key, so every `K` entries
the builder writes a *restart point*: an entry with `shared_len = 0`. The offsets of all restart
points sit at the end of the block, which lets a reader binary-search them. LevelDB's defaults are
`K = 16` (`block_restart_interval`) and a target block size of 4 KB of uncompressed data
(`block_size`).

<figure class="diagram">
{{#include figures/sstable-block.svg}}
<figcaption>A data block with six entries and, for illustration, <code>K = 3</code>. Each entry stores the length of the prefix it shares with the previous key and the rest of its key. "apple" and "apricot" are restart points with full keys, and the restart array at the end of the block holds their offsets, followed by their count. On disk the block is followed by its trailer, the compression type and CRC.</figcaption>
</figure>

### The index block

The index maps a key to the one data block that could contain it. Two conventions exist:

- **First key of each block** (as in the overview, and in Mini-LSM): find the last block whose
  first key is at most the search key.
- **A separator after each block** (LevelDB): the index key for block `i` is at least the last key
  of block `i` and smaller than the first key of block `i + 1`. Find the first block whose index
  key is at least the search key. LevelDB picks the shortest such separator, to keep the index
  small; the last key of the block is always a correct, simpler choice.

### Write

1. Take entries from a sorted source, usually a [memtable](memtable.md) or a
   [merge](k-way-merge-heap.md) of other SSTables, and reject any key that is not larger than the
   previous one.
2. Append each entry to the current data block, prefix-compressing it, and starting a restart
   point every `K` entries. Add the key's hash to the filter builder.
3. When the block reaches the target size, finish it: append the restart array and its count,
   optionally compress it, write it with its trailer, and remember its handle. Add an index entry
   for it.
4. After the last entry, finish the last block, even if it is small.
5. Write the filter block, then the metaindex block, then the index block, each with its trailer.
6. Write the footer, then sync the file.

The table becomes part of the database only when a manifest records it (see
[LSM tree](lsm-tree.md)). A crash before that leaves an orphaned file, not a corrupt database.

### Open

1. Read the last 48 bytes (the footer) and check the magic number.
2. Read and verify the index block, and the filter block if there is one, and keep both in
   memory.

### Point lookup

1. Ask the [Bloom filter](../filters/bloom-filter.md), if present; "definitely absent" ends the
   lookup.
2. Binary-search the in-memory index for the candidate block. If there is none, the key is
   absent.
3. Read that block, verify its checksum, and decompress it.
4. Binary-search the restart points, comparing the search key with each restart point's full
   key, to find the last restart point whose key is at most the search key.
5. Decode entries forward from there until one has a key at least the search key. It is a match
   only if equal.

With internal keys (see [memtable](memtable.md)), a lookup searches for `(user_key, snapshot)`
and checks the user key of the entry it lands on, exactly as a memtable lookup does.

### Scan

A scan is a *two-level iterator*: an iterator over the index entries, and for each, an iterator
over the entries of that block. Seeking positions both; reaching the end of a block moves the
index iterator and opens the next block.

### Variants

- **Simpler blocks.** A first implementation can drop prefix compression and restart points
  altogether: store full keys and an array of entry offsets at the end of each block. It is
  larger on disk but has the same structure.
- **Partitioned index and filters.** For very large files, RocksDB can split the index and the
  filter into partitions with a small top-level index, so they need not fit in memory whole.
- **Key-value separation.** Storing large values in a separate log and only pointers in the
  SSTable (WiscKey, Lu et al., 2016) makes compaction rewrite much less data.

## Invariants

- Keys are strictly increasing across the whole file (by the internal key comparator, when
  internal keys are used).
- Each index entry correctly separates its block from the next, under whichever convention the
  file uses.
- Within a block, restart offsets are increasing, point inside the entry area, and each points at
  an entry with `shared_len = 0`.
- Every block handle lies inside the file, and blocks do not overlap.
- Every block's checksum matches its contents and compression type.
- The footer is at the end of the file and holds the magic number.
- The file never changes after it is finished.

## Implementation pitfalls

- **Trusting lengths read from disk.** A corrupt varint or length can point past the block. Every
  decode step must check bounds (`get`, `split_at_checked`, `checked_add`) and return a corruption
  error. This crate's `record::decode` already follows that pattern; panicking on bad input is
  both a lint failure and a denial-of-service bug.
- **`shared_len` longer than the previous key.** Valid-looking varints can still describe an
  impossible entry. Check it before slicing the previous key.
- **Varints that never end.** A `u64` varint has at most 10 bytes; stop and report corruption
  after that rather than shifting past 64 bits.
- **Forgetting the last block.** A builder that only writes blocks when they fill up drops the
  tail of the table. Test a table whose last block holds a single entry.
- **Index off-by-one.** With the first-key convention, a key smaller than the first block's first
  key is absent, and a key larger than the last key lands in the last block, where the block
  search must still answer "absent".
- **Byte order.** Fixed-width integers in the format must have one defined byte order (LevelDB
  uses little-endian); `to_ne_bytes` makes files unreadable on other machines.
- **Checksums that skip the type byte.** If the checksum does not cover the compression type, a
  flipped type byte decompresses garbage without an error. LevelDB's checksum covers both.
- **Values larger than a block.** The target block size is a threshold, not a limit; a single
  large entry produces one oversized block, and the reader must accept it.
- **Comparator mismatch.** A file sorted with one comparator and searched with another returns
  wrong answers silently. LevelDB records the comparator's name in its manifest and refuses to
  open a database with a different one.
- **Borrowing from a block.** Decoding without copying means keys and values borrow from the
  block's bytes; an iterator that owns its current block and lends out slices from it fits Rust's
  lifetimes better than one that returns references outliving the block.

## Testing

- Round trip: write a random sorted map, read it back by iteration, and compare with the map.
- Point lookups: every stored key, plus absent keys before the first key, after the last, and
  between every pair of neighbours, including neighbours in different blocks.
- Seeks and ranges: compare scans from random start keys with `BTreeMap::range`.
- Parameters: a block size so small that each block holds one entry, a restart interval of 1, and
  keys that share long prefixes or are empty.
- Corruption: flip each byte of a small table in turn, and truncate it at every length; every read
  must return an error or a correct answer, never panic, and never return a wrong value from a
  block whose checksum is checked.
- Fuzzing: feed arbitrary bytes to the reader (for example with `cargo fuzz`); it must never
  panic.
- Edge cases: an empty table, a table of one entry, and a single entry larger than the block size.

## Further reading

- [LevelDB table format](https://github.com/google/leveldb/blob/main/doc/table_format.md). Data
  blocks, restart points, the index block and the footer.
- [Bigtable: A Distributed Storage System for Structured
  Data](https://research.google.com/archive/bigtable-osdi06.pdf), Chang et al., 2006.
- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. Block and SST encoding chapters.

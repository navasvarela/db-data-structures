# LSM tree

> Builds on [memtable](../lsm-trees/memtable.md), [SSTable](../lsm-trees/sstable.md), [k-way merge
> heap](../lsm-trees/k-way-merge-heap.md), [Bloom filter](../filters/bloom-filter.md) and
> [write-ahead log](../storage-primitives/write-ahead-log.md).

A memtable plus layers of SSTables. Background compaction merges SSTables to discard overwritten and
deleted keys. A manifest records which SSTables make up the current version.

- **Used by:** LevelDB, RocksDB, Cassandra, ScyllaDB, CockroachDB's Pebble.
- **Cost:** fast, sequential writes; a read may consult the memtable and one table per level.
- **Notes:** leveled compaction (each level about 10 times the size of the previous one) favours
  reads and space; tiered compaction favours writes. Every design trades off read, write and space
  amplification. See O'Neil et al., 1996.

## How it works

### Layout

An LSM tree turns random writes into sequential ones. Writes go to a log and an in-memory sorted
buffer; full buffers are written out as immutable sorted files; background merges keep the number
of files a read has to check small. This page follows LevelDB's design, which RocksDB extends.

<figure class="diagram">
{{#include figures/lsm-tree-layout.svg}}
<figcaption>Files drawn by the key range they cover. Each flush adds a file to level 0 (the newest, <code>b–f</code>, is highlighted), so level-0 files overlap. Levels 1 and 2 each split one sorted run into files with disjoint ranges, level 2 holding about ten times as much data as level 1.</figcaption>
</figure>

- **Level 0** receives flushed memtables as they are, so its files can overlap each other.
- **Levels 1 and above** each hold files with disjoint key ranges, sorted by key, so together a
  level is one long sorted run split into files. Each level has a target size; in LevelDB, level
  `L` holds about `10^L` MB, so each level is ten times larger than the one above.
- A *version* is the set of files in each level at one moment. The *manifest* is a log of the
  changes between versions, and the file `CURRENT` names the manifest in use.

### Sequence numbers and internal keys

Every write gets a *sequence number* from a counter that never goes backwards. Entries are stored
under *internal keys*, `(user key, sequence number, kind)`, sorted by user key ascending and then
by sequence number descending, where the kind is *put* or *delete* (a tombstone). See
[memtable](memtable.md) for the details. Sequence numbers give the whole tree one rule:

- For any user key, the version with the largest sequence number is the current one.
- Newer components hold larger sequence numbers for a given key: the memtable's versions are
  newer than level 0's, newer level-0 files' versions are newer than older ones', and level `L`'s
  are newer than level `L + 1`'s.

A *snapshot* is a sequence number `s`: a read at `s` ignores every entry with a larger sequence
number, so it sees the database as it was when `s` was assigned.

### Write path

1. Assign the next sequence number to the write (or a consecutive range to a batch).
2. Append it to the WAL, syncing if the caller asked for durability.
3. Insert it into the active memtable.
4. If the memtable is full, freeze it, start a new memtable and WAL, and schedule a flush.

### Point lookup

1. Look in the active memtable, then in the immutable memtable. Stop at the first put (found) or
   tombstone (absent).
2. Look in the level-0 files whose key range contains the key, **newest file first**. Check each
   file's [Bloom filter](../filters/bloom-filter.md) before reading a block.
3. For each level from 1 down, binary-search the level's files by key range to find the one file
   that can hold the key, check its filter, and look inside.
4. The first put or tombstone found, at any step, is the answer; nothing older is consulted. If
   nothing is found, the key is absent.

<figure class="diagram">
{{#include figures/lsm-tree-read-path.svg}}
<figcaption>A lookup for <code>h</code> in the tree above. After both memtables (1, 2), it checks the level-0 files whose range contains <code>h</code>, newest first: <code>c–p</code> (3), then <code>a–k</code> (4); <code>b–f</code> cannot hold it. In level 1 only <code>e–h</code> can, and the key is found there (5), so level 2's <code>g–h</code> is never read.</figcaption>
</figure>

A read touches every level-0 file that overlaps the key plus at most one file per deeper level,
which is why the number of level-0 files has to be kept small.

### Range scan

1. Create an iterator over each memtable, each level-0 file, and each deeper level (one iterator
   per level is enough, since its files are disjoint and sorted).
2. Merge them with a [k-way merge heap](k-way-merge-heap.md), newer sources winning ties.
3. For each user key, keep only the newest version visible at the snapshot, and skip the key if
   that version is a tombstone.

### Flush

1. Write the immutable memtable to a new [SSTable](sstable.md) and sync it.
2. Append a *version edit* to the manifest, "add this file to level 0", along with the number of
   the oldest WAL still needed, and sync the manifest.
3. Delete the old WAL, and drop the immutable memtable.

LevelDB may place a flushed table deeper than level 0, up to level 2, when it overlaps nothing
there.

### Leveled compaction

Compaction merges files to discard shadowed versions and tombstones, and to keep each level
within its target. LevelDB's version:

1. **Pick a level.** Level 0 needs compaction when it holds 4 or more files; level `L ≥ 1` when
   its size exceeds its target. LevelDB scores each level against its limit and compacts the
   highest score.
2. **Pick inputs.** From level `L`, take one file: the first file after the key where the last
   compaction of this level stopped, wrapping around. For level 0, add every other level-0 file
   that overlaps it. Then add every file of level `L + 1` whose range overlaps the inputs.
3. **Merge** the inputs with a k-way merge, and for each entry decide whether to drop it:
   - an entry is dropped if a newer version of the same user key has already been seen and that
     newer version is visible to the oldest live snapshot (it hides this one from every reader);
   - a tombstone is dropped if it is visible to the oldest snapshot and no level deeper than
     `L + 1` has a file whose range contains the key, since then nothing older can exist below
     for it to hide.
4. **Write outputs** as new SSTables in level `L + 1`, starting a new file when the current one
   reaches the target file size (2 MB in LevelDB) or would overlap too much of level `L + 2`
   (in LevelDB, ten times the target file size), which bounds the cost of the next compaction.
5. **Install:** sync the outputs, then append one version edit that removes the inputs and adds
   the outputs, and sync the manifest. This is the commit point.
6. **Collect garbage:** delete input files once no version that a reader still holds refers to
   them.

<figure class="diagram">
{{#include figures/lsm-tree-compaction.svg}}
<figcaption>Compacting level 1 into level 2. The input from level 1, <code>e–h</code>, overlaps <code>e–f</code> and <code>g–h</code> in level 2, so all three are merged. The outputs replace them in level 2 in a single version edit, and level 1 is left with a gap in its key range.</figcaption>
</figure>

### Tiered compaction

Instead of merging into an existing sorted run, tiering lets several runs accumulate at a level
and merges them all together into one run at the next level once there are enough of them.
RocksDB calls its variant *universal compaction*. With a size ratio `T` between levels, leveling
rewrites each entry about `T` times per level but leaves one run per level to search, while
tiering rewrites each entry about once per level but leaves up to `T` runs per level to search
(Luo and Carey, 2019). LevelDB's level 0 already behaves like a tier.

<figure class="diagram">
{{#include figures/lsm-tree-policies.svg}}
<figcaption>Leveling keeps one sorted run per level, each <code>T</code> times larger than the one above, and merges new data into it. Tiering lets up to <code>T</code> runs of similar size pile up in a level before merging them into one run of the next level.</figcaption>
</figure>

### Manifest and versions

A version edit records changes to the version: files added (with level, file number, size,
smallest and largest internal key), files deleted, the next file number, the last sequence
number, the current WAL number and the comparator's name. To change the version:

1. Write the edit to the manifest and sync it.
2. Apply it in memory to produce the new current version.

On open, a fresh manifest can be started by writing a snapshot of the whole version into a new
file, then pointing `CURRENT` at it. LevelDB writes the new name to a temporary file, syncs it, and
renames it over `CURRENT`, so a crash leaves either the old or the new pointer.

### Recovery

1. Read `CURRENT` to find the manifest, and replay its edits to rebuild the last version.
2. Replay every WAL newer than the one the manifest says is flushed, oldest first, into a
   memtable (flushing it if it fills up), and track the largest sequence number seen.
3. Set the sequence counter to the larger of the manifest's and the logs' last sequence numbers.
4. Delete files that no version references: SSTables from a compaction or flush that never
   committed, and WALs that were already flushed.

### Write stalls

If writes outrun compaction, level 0 grows and reads slow down. LevelDB pushes back on writers:
at 8 level-0 files it delays each write by 1 ms, at 12 it stops writes until compaction catches
up, and a writer whose memtable is full waits while the previous one is still being flushed.

### Variants

- **Range tombstones:** a single entry that deletes a whole key range, such as RocksDB's
  `DeleteRange`. Reads and compactions have to check every point entry against the ranges that
  cover it, which makes them considerably more complex.
- **Key-value separation** (WiscKey, Lu et al., 2016): values live in a separate log, so
  compactions move only keys and pointers; the cost is an extra read per lookup and a separate
  garbage collector for the value log.
- **Per-level filter sizing** (Monkey): giving deeper, larger levels fewer bits per key and
  shallower levels more lowers the total false-positive cost for the same memory.

## Invariants

- In every level from 1 down, files have disjoint key ranges and are ordered by smallest key.
- If two adjacent files of a level both hold versions of one user key, a compaction that takes
  one of them also takes the other (see Implementation pitfalls).
- For any user key, every version in a newer component has a larger sequence number than every
  version in an older one.
- The sequence counter is at least the largest sequence number in any memtable or file.
- Every file the current version references exists, is complete and synced; every file it does
  not reference is garbage, unless an ongoing compaction or a live reader still uses it.
- Every acknowledged write is in a memtable, in a WAL that has not been deleted, or in a file of
  the current version.
- A read at snapshot `s` returns, for each user key, the newest version with sequence number at
  most `s`, or nothing if that version is a tombstone.
- A tombstone is never dropped while an older version of its key may still exist in a deeper
  level, or while a snapshot could see the value it hides.

## Implementation pitfalls

- **Dropping tombstones too early.** Write `k = v1` and let it compact down to level 3. Delete
  `k`; the tombstone reaches level 1. If a compaction from level 1 into level 2 drops it because
  level 2 has nothing for `k`, the next read finds `v1` in level 3: the deleted value is back.
  Check every deeper level, not only the output level.
- **Searching level 0 in the wrong order.** Level-0 files overlap, so the newest must be searched
  first, by file number or sequence number, not by key range.
- **Splitting a user key across files.** If two adjacent files of a level hold versions of the
  same user key, and a compaction takes only the first file (with the newer version) down a
  level, a lookup then finds the older version first in the upper level. LevelDB's
  `AddBoundaryInputs` adds such neighbouring files to the compaction. An output writer that never
  ends a file in the middle of a user key's versions avoids creating the situation.
- **Deleting files too soon.** An input file may still be read by an iterator or snapshot that
  holds the old version. Reference-count versions, and delete a file only when no live version
  refers to it.
- **Commit order.** Sync the new SSTable before the manifest edit that references it, and sync the
  manifest before deleting WALs or input files. On POSIX file systems, also sync the directory
  after creating or renaming a file, or the new name itself may not survive a crash.
- **Reusing file numbers.** Persist the next file number in the manifest; otherwise, after a
  crash, a new file can be given the number of a file that still exists.
- **Restoring the sequence counter.** Missing it on recovery makes new writes lose to older ones.
- **Unbounded level 0.** Without write stalls, a fast writer creates level-0 files faster than
  compaction removes them, and every read checks all of them.
- **Shared state in Rust.** Readers, the writer and a background compaction thread share the
  current version. Keeping it as an `Arc<Version>` that is swapped under a short-lived `Mutex`
  lets each reader clone the `Arc` and work on its version without holding the lock, and dropping
  the last `Arc` is a natural trigger for deleting obsolete files. Send compaction requests over a
  channel, and never hold the lock across file I/O.
- **Arithmetic.** Level targets such as `10^L` MB overflow `u64` bytes once `L` is large enough;
  compute them with `checked_pow` and `checked_mul`. LevelDB's sequence numbers have 56 bits, so
  advancing them must check against that limit, not against `u64::MAX`.
- **Stable formats.** Everything on disk (filters, internal keys, the manifest) must use fixed
  byte orders and stable hashes, never `DefaultHasher` or `to_ne_bytes`.

## Testing

- Reference model: a `BTreeMap` driven by random puts, deletes, gets and range scans, with a tiny
  memtable and file size so that flushes and compactions happen every few operations. Compare
  every answer.
- Invariant checker: after each flush and compaction, walk the current version and assert the
  invariants above: disjoint ranges per level, sequence ordering across levels, and every
  referenced file present.
- Tombstone resurrection: the scenario under Implementation pitfalls, written as a test that
  forces each compaction step and checks the key is absent after every one.
- Snapshots: take a snapshot, overwrite and delete keys, compact everything, and check the
  snapshot still reads the old values while new reads do not.
- Crash simulation: run the engine on a storage abstraction that can lose everything not synced,
  in the spirit of this crate's `ByteStore` and `CompactionStores` traits. Crash at random points,
  including in the middle of a flush and a compaction, reopen, and check that every synced write
  is present and that no value appears that was never written.
- Determinism: run compactions inline on the writer's thread in tests, with a seeded random
  generator, so any failing sequence can be replayed exactly.

## Further reading

- [The Log-Structured Merge-Tree (LSM-Tree)](https://www.cs.umb.edu/~poneil/lsmtree.pdf), O'Neil et
  al., 1996.
- [LSM-based Storage Techniques: A Survey](https://arxiv.org/abs/1812.07527), Luo and Carey, 2019.
  The map of the design space: compaction policies, filters, and the trade-offs between them.
- [Monkey: Optimal Navigable Key-Value
  Store](https://cs-people.bu.edu/mathan/publications/sigmod17-dayan.pdf), Dayan, Athanassoulis
  and Idreos, 2017. Sizing Bloom filters per level.
- [RocksDB wiki](https://github.com/facebook/rocksdb/wiki). Leveled and universal compaction, the
  manifest, and tuning guides from a production engine.
- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. A complete LSM engine in Rust, built in three
  weeks of exercises.

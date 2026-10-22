# Memtable

The sorted, in-memory write buffer of an LSM tree. Writes land here first; when it reaches a size
threshold it is frozen and flushed to disk as an SSTable.

- **Used by:** every LSM engine.
- **Cost:** that of the ordered structure underneath it, usually `O(log n)`.
- **Notes:** deletes are written as tombstones, not removals, so they can shadow older values in
  SSTables. Without a [WAL](../storage-primitives/write-ahead-log.md) the memtable is lost on a
  crash.

## How it works

### Layout

A memtable is an ordered map plus a running estimate of its size. The map is any ordered structure
that supports inserts and in-order iteration: LevelDB and RocksDB use a
[skip list](../in-memory-indexes/skip-list.md) by default; this crate's
[red-black tree](../in-memory-indexes/red-black-tree.md) is the other natural choice.

An LSM engine keeps one *active* memtable that takes writes, and zero or more *immutable*
memtables that are full and waiting to be flushed. Each memtable has a matching log file, the
[write-ahead log](../storage-primitives/write-ahead-log.md) (WAL), that holds the same writes in
arrival order:

<figure class="diagram">
{{#include figures/memtable-lifecycle.svg}}
<figcaption>Each memtable has its own WAL. A write is appended to the active memtable's WAL, then inserted into the memtable. A full memtable is frozen and later flushed to an SSTable, after which its WAL (here WAL 6) can be deleted. Reads check the active memtable, then the immutable memtables from newest to oldest, then the SSTables.</figcaption>
</figure>

### Entries and internal keys

A memtable cannot store a delete by removing the key, because older values of that key may sit
in SSTables on disk and would reappear. It stores a *tombstone*, an entry that says "deleted".
So each entry has a kind, *put* or *delete*, and two designs are common:

- **One entry per key.** Map the user key to an enum such as `Put(value)` or `Tombstone`, and
  overwrite on every write. This is the simplest correct memtable and a good first version.
- **One entry per write.** Every write gets a *sequence number*, a counter that increases with
  each write across the whole database. The map key becomes an *internal key*: the user key, then
  the sequence number, then the kind. Internal keys sort by user key ascending, then by sequence
  number **descending**, so the newest version of a key comes first. Keeping old versions is what
  lets a reader use a *snapshot*: a sequence number below which it sees the database as it was.
  LevelDB packs a 56-bit sequence number and an 8-bit kind into the last 8 bytes of each internal
  key.

<figure class="diagram">
{{#include figures/memtable-internal-keys.svg}}
<figcaption>Internal keys in memtable order: three versions of "apple", newest first, then "pear". A lookup seeks to <code>("apple", snapshot)</code> and takes the first entry at or after it (see Point lookup below): at snapshot 15 it finds the put at 12, at snapshot 10 the tombstone at 7, and at snapshot 5 the older put at 3.</figcaption>
</figure>

### Write

1. Assign the write the next sequence number.
2. Append the write to the WAL, and sync it if the caller asked for durability.
3. Insert the entry into the active memtable: the value for a put, a tombstone for a delete.
4. Add the entry's size (key, value and a per-entry overhead) to the size estimate.
5. If the estimate is now over the threshold, freeze the memtable (see below).

The WAL comes first: a write that reached the memtable but not the log would be acknowledged and
then lost on a crash.

### Point lookup

1. With internal keys, seek to the first entry at or after `(user_key, snapshot_sequence)`.
   Because sequence numbers sort descending, that is the newest version visible to the reader.
2. If the entry's user key differs, the key is **not in this memtable**: continue in the next
   memtable or SSTable.
3. If it is a put, the key is **found** with that value.
4. If it is a tombstone, the key is **deleted**: stop and answer "absent" without looking at
   anything older.

The three outcomes have to stay distinct all the way up to the LSM read path.

### Freeze and flush

1. **Freeze:** mark the active memtable immutable, create a new empty active memtable with a new
   WAL file, and send the old pair to the flusher. Writes continue in the new memtable.
2. **Flush:** iterate the immutable memtable in order and write every entry, tombstones
   included, into a new [SSTable](sstable.md).
3. Sync the SSTable, then record it in the manifest (see [LSM tree](lsm-tree.md)).
4. Only then drop the immutable memtable and delete its WAL file.

If the flusher falls behind and an immutable memtable is still waiting when the active one fills
up, writes must wait. LevelDB allows one immutable memtable and blocks writers in that case.
LevelDB's default threshold, `write_buffer_size`, is 4 MB.

### Recovery

1. Read the manifest to learn which WAL files have not been flushed.
2. Replay each, oldest first, inserting every record into a fresh memtable. LevelDB flushes the
   memtable to a level-0 table during replay whenever it grows past the threshold.
3. Set the next sequence number to one past the largest sequence number seen, in the WAL or in
   the manifest.
4. Ignore a torn record at the very end of the last log: a crash interrupted its append, so it
   was never synced, and no write that asked for a sync was acknowledged before its record was
   complete. Report corruption anywhere else instead of silently skipping it.

## Invariants

- Iteration yields entries in internal key order: user key ascending, then sequence number
  descending.
- Sequence numbers are unique, and none exceeds the last one assigned.
- Every entry in a memtable is also in its WAL, written before the entry was inserted.
- An immutable memtable never changes after it is frozen.
- The size estimate never decreases while the memtable is active, and is at least the total size
  of its keys and values.
- A lookup checks the active memtable, then immutable memtables from newest to oldest, then
  SSTables, and stops at the first put or tombstone.

## Implementation pitfalls

- **Removing instead of tombstoning.** The memtable looks correct on its own, and older values
  reappear only once SSTables exist.
- **`Option` for the lookup result.** `None` cannot mean both "deleted" and "not here". Use a
  three-variant enum, such as `Found(value)`, `Deleted` and `Missing`, so callers cannot confuse
  them.
- **Sequence order in an encoded key.** Encoding the sequence number big-endian makes byte
  comparison sort the oldest version first. Encode `u64::MAX - sequence`, or compare with a
  custom comparator.
- **Sequence numbers after a restart.** If recovery does not restore the counter, new writes get
  numbers smaller than old ones already in SSTables, and the old values win.
- **Deleting the WAL too early.** Until the SSTable is synced and recorded in the manifest, the
  WAL is the only durable copy of those writes.
- **Size accounting.** Count bytes actually held (key and value lengths plus per-entry overhead),
  not the number of entries, or a few large values exhaust memory before a flush triggers. A
  single value larger than the threshold must still be accepted.
- **Sharing with readers in Rust.** Readers and the flusher need the immutable memtables while
  writers use the active one. Holding immutable memtables in an `Arc` lets each reader keep the
  ones it started with alive. A map whose `insert` takes `&self`, such as `crossbeam_skiplist`'s
  `SkipMap`, avoids a lock around the active one; a `RedBlackTree` needs a `RwLock` or a single
  writer thread.
- **Scans and tombstones.** A user-facing range scan must hide tombstones and older versions,
  but a flush must write every tombstone; a version that has been overwritten may be dropped only
  if no snapshot can still see it.

## Testing

- Reference model: a `BTreeMap<String, Option<String>>`, where `None` is a tombstone, driven by
  random puts and deletes; compare every lookup's three-way outcome and the full iteration.
- Version order: write several versions of one key and check that iteration yields them newest
  first, and that a lookup at each snapshot returns the version that was current then.
- Flush round trip: flush to an SSTable, read it back, and compare with the memtable's contents,
  tombstones included.
- Crash simulation: perform writes, drop the memtable without flushing, replay the WAL into a new
  one, and compare. Repeat with the WAL truncated at every byte offset of its last record, which
  must lose exactly that write and nothing else.
- Sequence continuity: after recovery, a new write to an existing key must shadow the old value.
- Freeze threshold: writes that cross the threshold exactly, and a single value larger than it.

## Further reading

- [Bigtable: A Distributed Storage System for Structured
  Data](https://research.google.com/archive/bigtable-osdi06.pdf), Chang et al., 2006. Section 5.3
  introduces the memtable, and section 5.4 compactions.
- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. Memtables, freezing and flushing, in Rust.
- [LevelDB implementation notes](https://github.com/google/leveldb/blob/main/doc/impl.md).

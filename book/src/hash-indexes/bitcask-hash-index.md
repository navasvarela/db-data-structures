# Bitcask hash index

> In this crate: `hash::simple::SimpleHashIndex`, in
> [`src/hash/simple.rs`](https://github.com/navasvarela/db-data-structures/blob/main/src/hash/simple.rs).

An in-memory hash table from each key to the offset of its latest record in an append-only log, with
compaction to drop overwritten records.

- **Used by:** Riak's Bitcask backend.
- **Cost:** `O(1)` lookups with at most one disk read. Every key must fit in memory, and there are
  no range queries.

## How it works

### Layout

The index has two parts: an [append-only log](../storage-primitives/append-only-log.md) on disk
holding every write ever made, and an in-memory hash table, the *keydir*, holding the location of
the latest record of each key.

In this crate the log is any `ByteStore`, usually `FileByteStore`. Each write becomes one log
record whose payload is the key and the value:

```text
log record:  ┌────────────────┬──────────────────────┬─────────┬────────────┐
             │ u32 length     │ u32 key length       │ key     │ value      │
             │ (byte store)   │ (backend::record)    │ (UTF-8) │ (bytes)    │
             └────────────────┴──────────────────────┴─────────┴────────────┘
              ◀────────────── size = 4 + 4 + key length + value length ─────▶
```

Writing `apple=red`, `pear=yellow` and then `pear=green` gives this keydir and log:

<figure class="diagram">
{{#include figures/bitcask-hash-index-keydir.svg}}
<figcaption>Each record's size counts the 4-byte length prefix, the 4-byte key length, the key and the value: 16 bytes for <code>apple=red</code>. The second write of <code>pear</code> moved its keydir entry to offset 34 and left the record at 16 dead.</figcaption>
</figure>

Storing the key inside the record is what makes the keydir rebuildable: the log alone holds
everything. Besides the map, the keydir keeps two counters: `live_bytes`, the total size of the
records the map points at, and `dead_bytes`, the total size of the records they replaced. Both
count the 4-byte length prefix, so they match the space actually used in the store.

The original Bitcask stores more per record and per key: each record has a CRC and a timestamp,
and each keydir entry records which file the record is in, since Bitcask keeps a directory of
files rather than one.

### Get

1. Look the key up in the keydir. If it is absent, return `None`.
2. Read the record at the stored offset: one positioned read for the length prefix and one for the
   payload.
3. Decode the payload with `record::decode`, and return the value.

A lookup never reads more than one record from disk. In this crate, `get` returns `None` and logs a
warning if the read or the decoding fails, rather than returning an error.

### Set

1. Encode the key and value into a payload, and compute the record's size. A key or record longer
   than `u32::MAX` bytes is rejected here, before anything is written.
2. Append the payload to the store and get back its offset.
3. Insert `key → (offset, size)` into the keydir. If the key had a previous location, move that
   location's size from `live_bytes` to `dead_bytes`.
4. Check whether compaction is due, and run it if so.

Step 3 is the commit point. If the append in step 2 fails, the keydir still points at the previous
record, which was never touched, so the key keeps its old value.

### Compaction

Overwritten records waste space. The index compacts when `dead_bytes > live_bytes`, which keeps the
store under twice the size of its live data. Each compaction copies the live bytes once, and only
happens after at least as many bytes have been overwritten since the last one, so its cost is
amortised over those writes.

1. Ask the `CompactionStores` for an empty store. For files, `FileCompactionStores` creates
   `<path>.compacting` next to the store, truncating any leftover from an earlier crash.
2. For each entry in the keydir, read its record from the old store, append it to the new one, and
   record the new offset in a fresh keydir with `dead_bytes = 0`.
3. Install the new store. For files this is three steps: `sync_all` the new file, rename it over the
   old path, and `sync_all` the parent directory so the rename itself is durable.
4. Only after installing succeeds, switch the index to the new store and the fresh keydir.

<figure class="diagram">
{{#include figures/bitcask-hash-index-compaction.svg}}
<figcaption>Writing <code>pear=blue</code> to the log above appends 16 bytes at offset 51. Dead bytes, 18 + 17 = 35, now exceed live bytes, 16 + 16 = 32, so the write triggers a compaction that copies the two live records into a store of exactly 32 bytes. The copy order follows the keydir's hash order, so it can differ from run to run.</figcaption>
</figure>

If any step fails, the index keeps using the old store and keydir, which are unchanged. On Unix the
old file handle keeps reading the old file even after the rename has replaced it at the path, so
this holds for a failure at the last step too.

A compaction triggered by `set` cannot fail that `set`: the write was already committed in step 3
of set, and reporting an error would make the caller think it was not. The error is logged
instead, and since `dead_bytes` is still above `live_bytes`, the next write tries again.

### Open and recovery

1. Scan the store from offset 0, in offset order.
2. For each record, decode its key and insert `key → (offset, size)` into an empty keydir, exactly
   as `set` does, including the dead and live byte counts.
3. If any record cannot be read or decoded, fail with `HashIndexError::CorruptRecord` or an I/O
   error, naming the offset.

Because a later record for a key replaces the earlier one during the replay, as it did when it was
written, the rebuilt keydir equals the one that existed before the restart.

### Crash safety

- **Crash during a set:** the record may be fully written, partly written, or absent. A torn
  record at the end of the file makes the next `open` fail with an error rather than silently
  return wrong data; the crate does not yet truncate a torn tail.
- **Crash during compaction:** before the rename, the path still holds the old, complete store, and
  the half-written `.compacting` file is ignored and later overwritten. After the rename, the path
  holds the complete, synced new store. There is no point at which the path holds a mix.
- **Power loss:** appends are not followed by `fsync`, so recent writes that were still in the
  operating system's page cache can be lost. Only compaction syncs.

### Variants

The paper's Bitcask differs from this crate in ways that are worth building next:

- **Deletes:** a delete appends a tombstone record. The keydir drops the key, and the next merge
  drops the tombstone. This crate's `HashIndex` trait has no delete yet.
- **Multiple files:** writes go to one active file, which is closed and becomes immutable once it
  reaches a size limit. Merging, Bitcask's word for compaction, rewrites only the immutable files,
  so it does not block new writes.
- **Hint files:** next to each merged file, Bitcask writes a hint file that holds each key with its
  value's position and size but not the value. Startup reads the hint files instead of the data
  files, which is much faster when values are large.
- **Checksums and sync on put:** each record carries a CRC, and the `sync_on_put` option syncs the
  active file after every write.

## Invariants

- For every key, the keydir points at the record most recently written for it, and reading that
  record returns the value from the last successful `set`.
- `live_bytes` equals the sum of the sizes of the records the keydir points at.
- As long as every append has succeeded, `live_bytes + dead_bytes` equals the size of the store,
  that is the offset the next append would return. (A file append that fails after reserving its
  region leaves a gap that neither counter includes.)
- After every `set` whose compaction succeeded, `dead_bytes ≤ live_bytes`.
- Right after a compaction, `dead_bytes = 0`, and the store holds exactly one record per key.
- Opening a store rebuilds a keydir with the same values and the same byte counts as before.
- A failed `set` changes neither the keydir nor the counters.

## Implementation pitfalls

- **Updating the keydir before the append.** If the keydir points at the new offset first and the
  append then fails, the key points at bytes that were never written. Append first, then update the
  map.
- **Copying records from a moving store.** Compaction reads the old store while building the new
  one. In this crate `set` and `compact` both take `&mut self`, so no write can happen in between.
  A concurrent version must send writes made during compaction to the new store, or copy them
  afterwards, or they are lost when the stores are switched.
- **Renaming without syncing.** A rename can reach the disk before the data of the file being
  renamed. Sync the new file before the rename, and sync the directory after it, or a crash can
  leave the path pointing at an empty or partial file.
- **Relying on Unix file semantics.** That the old handle keeps reading a file after another file
  has been renamed over its path is POSIX behaviour: the replaced file stays alive until its last
  handle closes. The crate's positioned reads already make it Unix-only; a port to another
  platform has to re-check this step.
- **Counting bytes without the prefix.** If sizes leave out the 4-byte length prefix, the dead and
  live counts drift away from the real file size, and compaction triggers late. The crate's
  `record_size` includes it.
- **Overflowing counters.** The byte counters are `u64` and use `saturating_add` and
  `saturating_sub`, because clippy denies arithmetic that can overflow in this crate.
- **Expecting a byte-identical compacted file.** The keydir is a `HashMap` with a random seed per
  process, so compaction copies records in a different order each run. That is harmless, because
  the keydir is never written to disk, but tests must not compare compacted files byte for byte.
  A hint file would be the first on-disk use of the keydir, and would need a stable order or a
  stable hash.
- **Memory per key.** Every key lives in memory as a `String`, plus a 16-byte `RecordLocation`,
  plus the hash map's own overhead. Millions of long keys can exhaust memory before the log gets
  large.
- **Lossy values.** Values are `String`, and `get` converts the stored bytes with
  `String::from_utf8_lossy`, so bytes written by another program that are not valid UTF-8 come back
  altered rather than as an error.

## Testing

- Reference model: apply random sequences of `set` with a small key space, so overwrites are
  frequent, to the index and to a `HashMap<String, String>`. After every step, every key must
  return the model's value.
- Reopen: after a random sequence, take the store out of the index, reopen it, and check that the
  values and the `(live_bytes, dead_bytes)` pair are unchanged. The crate's tests do this for
  memory stores, including after a compaction.
- Compaction: check that it triggers exactly when dead bytes first exceed live bytes, and that
  afterwards the next append offset equals `live_bytes`, which proves the store holds only live
  records.
- Failure injection: wrap a store so that appends, store creation, copying or installation fail on
  demand. A failed `set` must leave the previous value; a failed compaction must keep the triggering
  write and the old store. The crate's `FlakyStore` and `FlakyStores` test doubles do this.
- Corruption: append records that are not valid key-value payloads, and assert that `open` fails
  with `CorruptRecord` naming the right offset.
- Crash simulation on files: after writing, truncate the file at every byte inside the last record
  and reopen; delete or truncate the `.compacting` file at random points of a compaction and
  reopen. The path must always open to a consistent state, or fail with an error, and never to a
  mix of old and new.

## Further reading

- [Bitcask: A Log-Structured Hash Table for Fast Key/Value
  Data](https://riak.com/assets/bitcask-intro.pdf), Sheehy and Smith, 2010.
- Kleppmann and Riccomini, *Designing Data-Intensive Applications*, 2nd ed., O'Reilly, 2026, the
  storage and retrieval chapter, section on hash indexes.

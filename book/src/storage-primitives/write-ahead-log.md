# Write-ahead log

> Builds on `backend::file::FileByteStore`.

A log of intended changes, made durable before the change is applied to the main data structure.
After a crash, replaying the log redoes changes that reached the log but not the data. It turns many
random writes into one sequential, fsynced write per commit.

- **Used by:** PostgreSQL WAL, SQLite WAL mode, RocksDB and LevelDB logs, InnoDB redo log.
- **Cost:** one sequential append and fsync per commit, or per group of commits with group commit.
- **Notes:** needs per-record checksums to detect a torn tail, a log sequence number (LSN) per
  record, and a checkpoint so the log can be truncated. ARIES (Mohan et al., 1992) is the standard
  recovery design built on it.

## How it works

### Layout

A write-ahead log is an [append-only log](append-only-log.md) whose records describe changes to
some other structure: a memtable, or the pages of a B+ tree. Every record gets a log sequence
number (LSN), a number that increases with every record. A common choice, and PostgreSQL's, is to
use the record's byte position in the log as its LSN, so an LSN also says where to find the record.

<figure class="diagram">
{{#include figures/write-ahead-log-layout.svg}}
<figcaption>A log shared by two transactions, T1 and T2. Each record's <code>prev LSN</code> points back to the previous record of the same transaction. The last record has been written but not yet fsynced, so the flushed LSN is still 150.</figcaption>
</figure>

The fields of one record:

```text
┌──────────┬────────┬──────┬─────┬────────┬──────────┬───────────────┐
│ checksum │ length │ type │ LSN │ txn id │ prev LSN │ payload       │
└──────────┴────────┴──────┴─────┴────────┴──────────┴───────────────┘
```

The fields beyond checksum, length and payload depend on the design. A log that only rebuilds a
memtable needs none of them. A log for a page-based store with transactions, in the style of ARIES,
records the transaction, a pointer to that transaction's previous record (`prev LSN`) so its records
can be walked backwards, and the page the change applies to.

Alongside the log, the system tracks:

- **Flushed LSN:** the end of the part of the log known to be on disk.
- **Page LSN:** stored in the header of every data page, the LSN of the last record applied to it.
  PostgreSQL's `pd_lsn` is this field.

### The two rules

Everything else follows from two rules:

1. **Log before data.** A changed page may be written to disk only after the log records describing
   its changes are durable, that is when `page LSN ≤ flushed LSN`. Otherwise a crash could leave a
   change on disk with no record of it, which recovery can neither redo nor undo.
2. **Log before commit.** A transaction may be reported as committed only after all its records,
   including its commit record, are durable.

With these, the data pages themselves can be written whenever is convenient, in any order, which is
what lets a [buffer pool](buffer-pool.md) cache pages and write them back lazily.

### Append and commit

1. Encode the change as a record, compute its checksum, and append it to an in-memory log buffer,
   often a [ring buffer](ring-buffer.md). The record's position becomes its LSN.
2. Apply the change to the in-memory structure, and for a page, set its page LSN to the record's
   LSN.
3. On commit, append a commit record, write the buffer to the log file, and `fsync` it.
4. Advance the flushed LSN to the end of what was synced, then acknowledge the commit.

**Group commit.** An `fsync` costs the same whether it covers one record or a hundred, so commits
that arrive while a sync is in progress wait, and the next sync covers all of them. Each waiter is
released when the flushed LSN passes its commit record.

### Checkpoint

Without checkpoints the log grows forever and recovery replays all of it. A checkpoint establishes
a point before which the log is no longer needed.

- **Sharp checkpoint:** stop accepting writes, flush every dirty page or the whole memtable, write a
  checkpoint record, and delete the log before it. Simple, but it stalls writers. An LSM tree does
  the equivalent per memtable: once a memtable is flushed into an
  [SSTable](../lsm-trees/sstable.md), the log segment that fed it can be deleted.
- **Fuzzy checkpoint (ARIES):** write a begin-checkpoint record, then an end-checkpoint record
  holding the table of active transactions and the dirty page table, which maps each dirty page to
  its `rec LSN`, the LSN of the first record that dirtied it since it was last written. Pages are
  not flushed as part of the checkpoint. A fixed location on disk, the master record, points to the
  latest checkpoint.

The log can be truncated before the smallest `rec LSN` in the dirty page table, and, if there is
undo, before the first record of the oldest active transaction.

### Recovery: redo only

This is the recovery of an LSM memtable log, and of any store that never writes uncommitted changes
to its data files.

1. Open the log segments that are newer than the last flushed state, in order.
2. Read records one at a time. Check the length against the bytes left, then the checksum.
3. Apply each valid record to an empty memtable, in LSN order.
4. At the first record that is incomplete or fails its checksum, stop. If it is in the last segment
   and nothing valid follows it, it is a torn tail: truncate the file there. Otherwise report
   corruption.

<figure class="diagram">
{{#include figures/write-ahead-log-redo-only.svg}}
<figcaption>The two ways redo-only recovery can stop. Only a bad record with nothing valid after it, at the end of the last segment, is a torn write.</figcaption>
</figure>

### Recovery: ARIES

ARIES allows dirty pages of uncommitted transactions to reach disk (a policy called *steal*) and
does not force pages to disk at commit (*no-force*), so recovery has to both redo and undo. It runs
three passes:

1. **Analysis.** Start at the last checkpoint and scan forward, rebuilding the transaction table
   and the dirty page table as they were at the crash. Transactions without a commit record at the
   end are the losers.
2. **Redo.** Start at the smallest `rec LSN` in the dirty page table and scan forward, repeating
   history: for each record that changes a page, re-apply it if the page's on-disk page LSN is less
   than the record's LSN, then set the page LSN. Losers are redone too.
3. **Undo.** Roll back every loser, following `prev LSN` pointers backwards and always undoing the
   record with the largest LSN first. Each undo is itself logged as a compensation log record (CLR)
   carrying `undo next LSN`, the next record to undo. If the system crashes during undo, the next
   recovery redoes the CLRs and resumes from `undo next LSN`, so no change is undone twice.

<figure class="diagram">
{{#include figures/write-ahead-log-aries.svg}}
<figcaption>Recovery after a crash that left T3 without a commit record. The checkpoint recorded P2 as dirty since LSN 30, so redo starts there, and the log before it could already have been truncated. Analysis starts at the checkpoint; undo walks T3's records backwards, from 80 to 60.</figcaption>
</figure>

### Variants

- **Physical, logical and physiological records.** A physical record holds byte images of a page
  region; a logical record holds an operation such as "insert key k". ARIES uses physiological
  records, which name a page and describe a logical operation within it.
- **Full-page writes.** A page larger than the disk's atomic write unit can be torn by a crash, and
  a physiological record cannot be replayed onto a torn page. PostgreSQL logs a whole image of a
  page the first time it changes after a checkpoint; InnoDB writes pages to a doublewrite buffer
  before writing them in place.
- **SQLite WAL mode.** Changed pages are appended to a separate `-wal` file instead of the database,
  readers look pages up in the WAL first, and a checkpoint copies pages back into the database
  file. By default it checkpoints when the WAL reaches 1000 pages.
- **Block-framed records.** LevelDB and RocksDB split records into 32 KiB blocks, with a 7-byte
  header per fragment and a record type of FULL, FIRST, MIDDLE or LAST. A reader that finds a bad
  block can skip to the next block boundary.

## Invariants

- LSNs strictly increase in log order, and the flushed LSN never decreases.
- For every page on disk, `page LSN ≤ flushed LSN`.
- Every acknowledged commit has its commit record below the flushed LSN.
- Every record below the flushed LSN has a valid checksum and decodes.
- After redo-only recovery, the memtable equals the result of applying every record in the log, in
  order, to an empty one.
- Recovery is idempotent: recovering, crashing during recovery, and recovering again gives the same
  state as recovering once.
- Truncation never removes a record that recovery could still need: nothing at or after the
  smallest `rec LSN`, nor any record of an active transaction.

## Implementation pitfalls

- **Retrying a failed `fsync`.** On Linux, when write-back fails, the kernel can mark the affected
  pages clean, so a second `fsync` returns success although the data never reached the disk. Since
  2018, PostgreSQL treats a failed `fsync` as fatal and recovers from the log instead. Do the same:
  return the error and do not acknowledge any commit after it.
- **Data still in a `BufWriter`.** `BufWriter` holds bytes in user space, so `sync_data` on the
  underlying `File` does not see them. Call `flush` first. Dropping a `BufWriter` flushes it but
  ignores errors, and dropping a `File` ignores close errors, so never rely on drop for durability.
- **New segment files that disappear.** Creating a log segment adds a directory entry, and that
  entry is only durable once the directory itself is synced. Open the directory and call
  `sync_all` on it after creating or renaming a segment, as `FileCompactionStores::install` does.
- **Zeros that pass the checksum.** A file preallocated with zeros, or a hole left by a crash, can
  look like a record with length 0 and checksum 0, and CRC32C of no bytes is 0. Make the checksum
  cover a non-zero field such as the type or the LSN; LevelDB reserves type 0 for preallocated
  space.
- **A checksum that does not cover the length.** If the length is outside the checksum, a corrupt
  length makes the reader verify the wrong bytes. Check the length against the bytes remaining
  first, then verify a checksum that covers both header and payload.
- **Treating any bad record as the tail.** Only an invalid record at the end of the last segment is
  a torn write. A bad record followed by valid ones is corruption, and silently truncating there
  discards committed data.
- **Non-idempotent redo.** Replaying "increment counter" twice gives the wrong answer. Either make
  every record idempotent, or compare page LSNs so a record is applied at most once per page.
- **Acknowledging too early under group commit.** Release a waiting commit only when the flushed
  LSN has passed its own commit record, not when any sync completes.
- **LSNs that restart.** If the LSN is a byte offset, it must be a global offset across segments
  that never resets, or a page LSN written before a truncation compares wrongly after it. Use a
  `u64`; in this crate, advance it with `checked_add`.
- **Decoding headers without panics.** Clippy denies slice indexing in this crate, so parse the
  header with `split_first_chunk` or `get`, and convert lengths with `u32::try_from` and
  `usize::try_from`, as the record decoder in `backend::record` does.

## Testing

- Reference model: apply the same random operations to a `BTreeMap` and to the WAL plus memtable.
  After a clean shutdown and recovery, the two must be equal.
- Crash simulation: put the file behind a test double that records every write and every sync. To
  simulate a crash, keep everything up to the last sync, then keep an arbitrary prefix of the later
  writes, cut at any byte. After recovery the state must contain every acknowledged write, and must
  equal the model after some prefix of the issued operations, never a state with gaps.
- Torn tail at every byte: write a few records, then for every length from the full size down to
  zero, truncate a copy of the file to that length and recover. Each must succeed, and must recover
  exactly the records that are complete.
- Corruption: flip one byte in a record that is followed by valid records, and assert that recovery
  reports an error instead of truncating.
- Idempotence: run recovery, interrupt it at random points, run it again, and compare with a single
  uninterrupted recovery.
- Checkpoints: recover after a checkpoint and truncation, and assert the result matches recovering
  the full log.

## Further reading

- [ARIES: A Transaction Recovery Method Supporting Fine-Granularity Locking and Partial Rollbacks
  Using Write-Ahead Logging](https://web.stanford.edu/class/cs345d-01/rl/aries.pdf), Mohan et al.,
  1992. The reference design for WAL-based recovery.
- [Write-Ahead Logging](https://sqlite.org/wal.html), SQLite documentation. A short, concrete WAL
  with checkpoints.
- [The Internals of PostgreSQL, chapter 9: Write Ahead
  Logging](https://www.interdb.jp/pg/pgsql09.html), Suzuki. LSNs, full-page writes and checkpoints
  in a real system.
- [LevelDB log format](https://github.com/google/leveldb/blob/main/doc/log_format.md). Records split
  across 32 KiB blocks, each with a checksum, so a torn tail is detected.

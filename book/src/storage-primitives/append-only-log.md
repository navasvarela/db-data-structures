# Append-only log

> In this crate: `backend::file::FileByteStore`, `backend::memory::MemoryByteStore`, in
> [`src/backend/file.rs`](https://github.com/navasvarela/db-data-structures/blob/main/src/backend/file.rs),
> [`src/backend/memory.rs`](https://github.com/navasvarela/db-data-structures/blob/main/src/backend/memory.rs).

A file that only ever grows. Writes go to the end, and a record is addressed by its offset. Appends
are sequential I/O, which is the fastest write a disk offers, and an interrupted write can only
damage the tail.

- **Used by:** Bitcask, Kafka partitions, the commit log of most LSM engines.
- **Cost:** `O(1)` append and offset read. Space grows with every overwrite until it is compacted.
- **Notes:** this crate length-prefixes each record and replays the log in offset order to rebuild
  indexes on open.

## How it works

### Layout

A log is a sequence of records laid end to end, starting at offset 0. The bytes of a record do not
describe where it begins, so each record carries its own length, and a reader finds record `n + 1`
by adding the size of record `n` to its offset. A record's address is the offset of its first
byte, and that address never changes for as long as the file exists.

In this crate every record is a 4-byte little-endian `u32` length prefix followed by that many
payload bytes. The store does not know what the payload means; the
[hash index](../hash-indexes/bitcask-hash-index.md) puts a key and a value inside it.

<figure class="diagram">
{{#include figures/append-only-log-layout.svg}}
<figcaption>Length prefixes are highlighted. Each arrow is one step of a scan: the next record starts 4 bytes plus the payload length further on.</figcaption>
</figure>

The payloads above are `"first"`, an empty payload, and `"third record"`, which the crate's tests
append and read back at offsets 0, 9 and 13. Besides the file, the store holds one number in
memory: `end`, the offset where the next record will go. On open, `end` is set to the file's
length.

`MemoryByteStore` keeps the same addresses without a file. It stores each payload as its own
allocation in a lock-free ordered map (a `crossbeam` skip map) keyed by offset, and it advances
`end` by the payload length plus 4, as if a prefix had been written. Both stores therefore hand out
the same offsets for the same sequence of appends, so code above them can be tested against
either.

### Append

1. Compute the record's size, `4 + payload length`. Reject a payload longer than `u32::MAX`
   bytes, because its length would not fit the prefix.
2. Reserve a region by advancing `end` atomically from `offset` to `offset + size`, failing if the
   addition overflows `u64`. Concurrent appenders each get a distinct, non-overlapping region.
3. Build the prefix and the payload in one buffer and write it at `offset` with a single
   positioned write (`write_all_at`, which is `pwrite` on Unix).
4. Return `offset`. The caller keeps it as the record's address.

<figure class="diagram">
{{#include figures/append-only-log-append.svg}}
<figcaption>Two concurrent appends after the three records above. Each reserved its region by advancing <code>end</code>; B has finished writing, A has not. Until A writes, its region holds zeros.</figcaption>
</figure>

The append does not call `fsync`, so a returned offset means the bytes reached the operating
system's page cache, not the disk. They survive a crash of the process but not a power loss.

### Read by offset

1. Read 4 bytes at `offset` and decode the length `len`.
2. Read exactly `len` bytes at `offset + 4` and return them.

An offset that does not point at the start of a record is not detected: the store reads whatever
4 bytes sit there as a length. Only the caller, which got the offset from `append`, knows it is
valid.

### Scan

Replaying the log is how anything built on it recovers its in-memory state.

1. Take a snapshot of `end`, and start at offset 0.
2. While the offset is below the snapshot, read the record there and yield it with its offset.
3. Move to `offset + 4 + len`.
4. If a read fails, yield the error and stop. Without a readable length there is no way to find
   the next record.

Records come out in ascending offset order, which is the order they were appended in. The
`ByteStore` trait documents this order as part of its contract, because a replay relies on a later
record overriding an earlier one.

### Recovery of the tail

A crash during step 3 of an append can leave a torn record: the prefix may be complete while the
payload is cut short, or the file may end inside the prefix itself. A scan reads such a record as
an unexpected end of file. In this crate the scan reports it as an error and stops; it does not
truncate the file. Truncating the torn tail before the next append, and telling a torn tail apart
from corruption in the middle of the file, is the job of the
[write-ahead log](write-ahead-log.md), which adds a checksum to every record for that purpose.

<figure class="diagram">
{{#include figures/append-only-log-torn-tail.svg}}
<figcaption>The log above after a crash that cut the file at byte 25. The prefix at 13 promises 12 payload bytes but only 8 remain, so the scan stops with an error there.</figcaption>
</figure>

### Variants

- **Segmented log:** instead of one file, a directory of files, each closed and made read-only
  when it reaches a size limit. Bitcask, Kafka and LSM commit logs all work this way, because old
  segments can then be compacted or deleted as whole files.
- **Block-framed log:** records are packed into fixed-size blocks, so a reader can resynchronise
  at the next block boundary after a damaged record. LevelDB's log uses 32 KiB blocks.
- **In-place overwrite:** the crate's `ByteStore` trait also has `write_record_at`, which writes a
  record at a chosen offset. Nothing in the hash index uses it; it exists for structures that
  update fixed locations.

## Invariants

- `end` never decreases, and equals the sum of the sizes of all records appended since the file
  was created.
- Every offset returned by `append` is distinct, and the regions `[offset, offset + size)` of two
  appends never overlap.
- Reading at an offset returned by a successful `append` returns exactly the payload that was
  appended, for as long as the store exists.
- A scan of a store that nothing is writing to yields every record once, in strictly ascending
  offset order, and the offset of each record equals the previous offset plus the previous size.
- Bytes before `end` are never changed by an append.

## Implementation pitfalls

- **Assuming an append is durable.** `write_all_at` returning `Ok` only means the kernel accepted
  the bytes. Durability needs `File::sync_data` or `File::sync_all`, and both are expensive, which
  is why real logs batch many appends per sync. On Apple platforms the standard library implements
  both with `fcntl(F_FULLFSYNC)`, which is much slower than `fsync` on Linux; benchmark on the
  platform you care about.
- **Appending after a torn tail.** On open, `end` is the file length, which includes the bytes of
  a torn record. Appending there buries the torn record in the middle of the log, and every later
  scan reads its bad length prefix and loses its place. Truncate the file to the end of the last
  complete record before appending.
- **Holes from concurrent appends.** Reservations are handed out in order, but the writes that fill
  them can finish in any order. If the process dies after a later region is written and before an
  earlier one is, the file contains a run of zero bytes that a scan reads as empty records, and,
  if the run is not a multiple of 4 bytes long, then as a length made of the leftover zeros and
  the next record's prefix. A checksum per record, or a single writer thread, closes this gap.
- **Reading a record that is still being written.** A reader that gets an offset from another
  thread before the append's write has finished can read zeros or a partial record. The crate's
  scan documents this: an append racing with it can show up as an empty or failed record.
- **Overflowing offsets and lengths.** In this crate clippy denies arithmetic that can overflow,
  so `end + size` and `offset + 4` are `checked_add` calls that turn overflow into an
  `InvalidInput` error, and the payload length is converted to `u32` with `try_from`.
- **Positioned I/O is platform-specific.** `read_exact_at` and `write_all_at` come from
  `std::os::unix::fs::FileExt`. Windows has `seek_read` and `seek_write` instead, with different
  semantics (they move the file cursor), so a portable store needs a shim.
- **Trusting the length prefix.** A corrupt prefix can claim up to 4 GiB, and the reader allocates
  a buffer of that size before it reads. Check the length against the remaining file size, or
  against a maximum record size, before allocating.

## Testing

- Round trip: append a list of payloads, including an empty one, then read each back at its
  returned offset and compare. Check the offsets themselves: they are the running sum of
  `4 + len`.
- Scan order: assert that a scan yields exactly the appended records, with their offsets, in
  order. Repeat after reopening the file to check persistence.
- Equivalence of backends: run the same sequence of operations against `FileByteStore` and
  `MemoryByteStore` and assert that they return the same offsets and payloads. A property test
  that generates random payload lists does this cheaply.
- Torn tail: append two records, truncate the file by a few bytes with `File::set_len`, reopen, and
  assert that the scan yields the first record and then an `UnexpectedEof` error. Repeat for every
  truncation point inside the last record, including inside the prefix.
- Concurrency: append from several threads at once and assert that all offsets are distinct, that
  every payload reads back intact, and that `end` equals the sum of all record sizes.
- Overflow: start a store with `end` near `u64::MAX` and assert that an append fails without
  changing `end` or storing anything.

## Further reading

- [Bitcask: A Log-Structured Hash Table for Fast Key/Value
  Data](https://riak.com/assets/bitcask-intro.pdf), Sheehy and Smith, 2010. Six pages, and the
  design this crate's hash index follows.
- Kleppmann and Riccomini, *Designing Data-Intensive Applications*, 2nd ed., O'Reilly, 2026, the
  storage and retrieval chapter, which builds a log-structured store up from a two-line shell
  script.
- Kreps, *The Log: What every software engineer should know about real-time data's unifying
  abstraction*, 2013. The log as the backbone of replication and stream processing.

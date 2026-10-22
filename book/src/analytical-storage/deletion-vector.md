# Deletion vector

> Builds on [bitmap index](../analytical-storage/bitmap-index.md).

A bitmap per immutable data file marking which row positions are deleted. Readers mask those rows
out, so deletes (and updates, as delete plus insert) need not rewrite the file.

- **Used by:** Delta Lake deletion vectors (stored as Roaring bitmaps), Apache Iceberg positional
  deletes, Vertica delete vectors, ClickHouse lightweight deletes.
- **Cost:** one bit per row at most; reads pay a mask per batch until a compaction rewrites the
  file.

## How it works

### Layout

A data file is immutable, and every row in it has a position: its 0-based index in the file,
counted across all row groups. A deletion vector (DV) is a set of positions, stored as a
[Roaring bitmap](bitmap-index.md), and the table's metadata pairs each data file with at most one
DV. The pair is the logical file: the data file's rows minus the positions in the DV.

```text
data file  part-0007.parquet   rows 0 … 999,999            (never modified)

DV         64-bit Roaring bitmap
             key 0 ──▶ 32-bit Roaring {3, 4, 7, 11, 18, 29}

metadata   (path = part-0007.parquet,
            dv = {file, offset, size in bytes, cardinality = 6})
```

Real formats differ in where the bitmap lives:

- **Delta Lake.** The `add` action for a file carries a deletion vector descriptor: a storage type
  (`i` inline in the log, `u` a file named from a UUID, `p` an absolute path), the inline data or
  path, an offset, the size in bytes and the cardinality. A DV file starts with a version byte,
  then for each DV a 4-byte size, the bitmap, and a CRC-32 checksum. The bitmap is a magic number
  (1681511377) followed by the 64-bit "portable" Roaring format.
- **Iceberg v3.** A DV is a `deletion-vector-v1` blob in a Puffin file: a 4-byte length, the magic
  bytes `D1 D3 39 64`, the same portable Roaring format, and a CRC-32. Those four bytes read as a
  little-endian integer are the same 1681511377. The manifest entry records the file, offset, length
  and referenced data file. At most one DV may exist per data file in a snapshot.

The portable 64-bit format splits each position into a 32-bit key (high half) and a 32-bit
Roaring bitmap of low halves, so files below 2^32 rows use a single key, 0.

### Delete rows

To delete the rows matching a predicate:

1. Plan the scan: use the [table manifest](table-snapshot-manifest.md) and [zone
   maps](zone-maps.md) to find files that may hold matching rows.
2. Scan each candidate file with its current DV applied, and collect the positions of matching rows.
3. For each file with new matches, load its current DV (or start from an empty one), add the new
   positions, and serialise the result to a new DV. Never modify the old DV in place; snapshots
   that reference it may still be read.
4. Commit atomically: replace each `(file, old DV)` with `(file, new DV)` in the table metadata.
   The data files themselves are untouched.

<figure class="diagram">
{{#include figures/deletion-vector-commit.svg}}
<figcaption>A delete that adds positions 11, 18 and 29 to a file whose DV already holds 3, 4 and 7. It writes a new DV and commits a snapshot that pairs the unchanged data file with it. The old DV is not modified, so readers of snapshot N still see their version.</figcaption>
</figure>

An update is a delete of the old rows plus an insert of their new versions into a new data file,
in the same commit.

### Read

1. For each file in the snapshot, load its DV: decode it inline, or read `size` bytes at `offset`
   from the DV file. Check the magic number, the size and the checksum.
2. Scan the file in batches. For a batch covering positions `[start, start + len)`, iterate the
   DV's set positions in that range and clear them in the batch's selection vector (see [columnar
   batch](../query-execution/columnar-batch.md)).
3. Positions are file-wide, so a reader that scans row group `g` must know the row group's first
   position, the sum of the row counts before it.
4. Answer `COUNT(*)` without reading rows: the file's row count minus the DV's cardinality. Delta
   requires `numRecords` to be the physical row count of the file for this reason.

<figure class="diagram">
{{#include figures/deletion-vector-mask.svg}}
<figcaption>The first 32 positions of <code>part-0007.parquet</code> read with its DV. The six set positions are cleared from the scan's selection, and the data file itself is never rewritten.</figcaption>
</figure>

### Purge

DVs make deletes cheap and reads slightly slower, and deleted rows still occupy space. A
maintenance job eventually rewrites heavily deleted files:

1. Pick files whose deleted fraction (cardinality over row count) passes a threshold.
2. Read each with its DV applied and write the surviving rows to a new file.
3. Commit, in one transaction, the removal of the old file with its DV and the addition of the new
   file with no DV.

### Variants

- **Positional delete files.** Iceberg v2 records deletes as rows of `(file_path, pos)`, sorted by
  path then position, in separate files that can cover many data files. Readers must merge every
  relevant delete file. Iceberg v3 forbids adding new ones and replaces them with DVs.
- **Equality deletes.** Iceberg v2 and v3 can also delete by column value (`id = 5`), which is
  cheap to write but has to be checked against every older row at read time.
- **A mask column.** ClickHouse's lightweight `DELETE` writes a hidden `_row_exists` column for
  the affected parts and adds a filter on it to queries; background merges remove the rows later.
- **Versioned deltas.** Kudu records deletes, with their MVCC timestamps, in delta stores keyed by
  row offset (see [delta and main store](delta-and-main-store.md)), so a scan applies only the
  deletes visible at its snapshot.

## Invariants

- Every position in a DV is less than its data file's row count.
- The stored cardinality equals the bitmap's cardinality.
- At most one DV is attached to a data file in any snapshot.
- Within one data file's history, each delete produces a DV that is a superset of the previous one.
- Reading a file with its DV returns exactly the rows that rewriting the file without the deleted
  rows would contain, in the same order.
- `visible rows = file row count - DV cardinality`.

## Implementation pitfalls

- **Mixed endianness.** In both Delta and Iceberg the Roaring bitmap is little-endian while the
  surrounding length and CRC fields are big-endian. Iceberg's specification says it chose big-endian
  for compatibility with Delta's existing DVs.
- **What the checksum covers.** Check your specification for exactly which bytes go into the CRC
  (Iceberg's covers the magic and the bitmap, not the length). A CRC computed over the wrong span
  fails on every file written by another implementation.
- **Row-group-relative positions.** Using the position within a row group instead of within the
  file deletes the wrong rows in every row group but the first, and tests with one row group pass.
- **Lost updates.** Two concurrent deletes on the same file both read DV version 1 and write DV 2a
  and DV 2b. If the second commit blindly replaces the DV, the first delete is undone. The commit
  must detect that the file's DV changed since it was read, and retry by merging the new positions
  into the current DV.
- **Rewrites invalidate positions.** Compaction, re-sorting or any rewrite of a data file changes
  every position. Its DV must be applied and dropped in the same commit as the rewrite, never
  carried over.
- **Stale statistics.** After deletes, a file's min and max may describe rows that no longer exist.
  They are still valid bounds for pruning, but not exact answers: `MIN(x)` cannot be read from them.
  Delta marks such statistics with `tightBounds = false`.
- **Applying the mask row by row.** Calling `contains(pos)` for every row is slow. Iterate the
  bitmap's positions within the batch range once, which touches only deleted rows.
- **64-bit keys.** Both formats require the most significant bit of the 64-bit position to be 0, so
  readers that use signed integers never see negative keys.

## Testing

- Model-based: keep a reference table as a `Vec` of rows with a deleted flag (or a `Vec<bool>` per
  file), apply random deletes, updates and purges to both, and compare full scans after each step.
- Known vector: Delta's protocol document includes an inline DV, encoded in Z85, whose positions it
  lists (3, 4, 7, 11, 18 and 29). Decode it and assert those positions.
- Serialisation round trip, including positions at and above `2^32`, and corruption tests that flip
  bytes in the magic, length, bitmap and checksum.
- Multi-row-group files: delete rows in the second and later row groups and check that exactly
  those rows disappear.
- Edge cases: deleting no rows, every row, row 0 and the last row; deleting a row twice; and a
  concurrent-delete test where two writers start from the same snapshot and both deletes survive.

## Further reading

- [Delta Lake transaction log protocol](https://github.com/delta-io/delta/blob/master/PROTOCOL.md).
  The deletion vector section specifies the on-disk format.
- [Apache Iceberg table specification](https://iceberg.apache.org/spec/). Positional delete files
  and deletion vectors.

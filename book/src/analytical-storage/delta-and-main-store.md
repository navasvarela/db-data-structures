# Delta and main store

> Builds on [columnar segments](../analytical-storage/columnar-segments.md) and [deletion
> vector](../analytical-storage/deletion-vector.md).

A compressed, sorted columnar layout is expensive to change in place. Writes go instead to a small
write-optimised store (row-wise or in memory), which is periodically merged into the read-optimised
main store. Queries read both and combine the results.

- **Used by:** C-Store's write-optimised and read-optimised stores, SAP HANA's delta merge, Apache
  Kudu (DeltaMemStores over DiskRowSets).
- **Notes:** the analytical counterpart of an [LSM tree](../lsm-trees/lsm-tree.md). ClickHouse is
  the LSM version directly: each insert writes a new sorted part, and background merges combine
  parts.

## How it works

### Layout

<figure class="diagram">
{{#include figures/delta-and-main-store-layout.svg}}
<figcaption>Writes land in the delta store; a periodic merge folds it into the main store, whose changes to existing rows are recorded beside it (red). A scan reads both at one snapshot.</figcaption>
</figure>

The main store is a set of [columnar segments](columnar-segments.md). It is never edited in place:
a change to it is recorded beside it, either as a mark in a [deletion vector](deletion-vector.md)
or as a delta record keyed by the row's position. The delta store holds new rows, and is designed
for cheap single-row writes and lookups.

Two published designs show the range of choices:

- **HYRISE**, the research system in Krueger et al. (SAP HANA's column store uses the same main
  and delta split): every column has a main part, dictionary-compressed with a sorted dictionary,
  and an uncompressed delta part with a CSB+ tree over its distinct values. Updates are
  insert-only: an update inserts a new version into the delta and invalidates the old row.
- **Kudu**: inserts go to an in-memory, row-wise MemRowSet, which is flushed to columnar
  DiskRowSets. A DiskRowSet's base data is immutable; updates and deletes to it are stored in delta
  stores as `(row offset, timestamp) → change list` records, first in memory (DeltaMemStore) and
  then flushed to DeltaFiles.

### Insert

1. Append the row to a [write-ahead log](../storage-primitives/write-ahead-log.md), so the delta
   survives a crash.
2. If the table has a unique key, check that the key exists neither in the delta nor in the main
   store. Kudu narrows the check with each DiskRowSet's key bounds and a Bloom filter, then confirms
   with the primary key column's index.
3. Add the row to the delta store.

### Update and delete

For a row still in the delta, change it there. For a row in the main store:

1. Find its position in the main store: look up the key (with a [sparse primary
   index](sparse-primary-index.md) or the main store's own key index) to get the row offset.
2. Record the change against that position. In the insert-only design, set the row's bit in the
   deletion vector and, for an update, insert the new version into the delta. In Kudu's design,
   append a change record for `(row offset, timestamp)` to the DiskRowSet's delta store.

An update that changes the sort key always becomes a delete plus an insert, since the row has to
move.

### Scan

1. Pick one snapshot (a timestamp or version) for the whole scan, so main and delta are read
   consistently.
2. Scan the main store with the usual pruning and late materialisation, masking deleted positions,
   or applying the delta records visible at the snapshot to each batch. Kudu stores deltas by row
   offset so this needs no key comparisons.
3. Scan the delta store and filter it with the same predicate.
4. Combine the two. Rows never appear in both, so a union is enough for most operators; a query that
   needs key order merges the two sorted streams.

### Merge

The merge folds the delta into a new main store. Following HYRISE's dictionary merge for one
column:

1. Freeze the current delta and direct new writes to a fresh, empty second delta.
2. Extract the frozen delta's distinct values in sorted order (a walk over the leaves of its CSB+
   tree) to form the delta dictionary.
3. Merge the main dictionary and the delta dictionary into one sorted dictionary without
   duplicates, recording for each old code its new code (one translation table for the main
   dictionary, one for the delta's).
4. Compute the new code width: `ceil(log2(new dictionary size))` bits. It may grow by a bit.
5. Write the new main column: the main's codes rewritten through the translation table, followed
   by the delta's values encoded with the new dictionary. HYRISE keeps invalidated rows, since its
   insert-only design lets queries read old versions; a design without history can drop them here
   and must then remap positions (see the pitfalls).
6. Atomically swap in the new main, make the second delta the current delta, and release the old
   main and frozen delta when no running scan uses them.

<figure class="diagram">
{{#include figures/delta-and-main-store-merge.svg}}
<figcaption>Steps 3 and 4 for one column. The sorted delta dictionary is merged into the main one, producing a translation table for each. Six values no longer fit in 2-bit codes, so the new main column uses 3 bits, and every old main code is rewritten through its table.</figcaption>
</figure>

Kudu's version is several smaller jobs: flushing a DeltaMemStore to a DeltaFile, compacting deltas
into the base data of the columns they touch, and compacting overlapping DiskRowSets into new ones,
which drops deleted rows.

When to merge is a policy: HYRISE triggers it when the delta's row count passes a set fraction of
the main's. A larger delta makes writes cheaper and merges rarer, but every scan pays for reading
the uncompressed delta.

### Variants

- **C-Store** has a writeable store and a read-optimised store, and a tuple mover that moves batches
  of records from the first to the second.
- **ClickHouse** has no single delta: every insert writes a small sorted part, and background
  merges combine parts, as in an [LSM tree](../lsm-trees/lsm-tree.md).
- **Table formats** (Delta Lake, Iceberg) offer the same trade-off per write: copy-on-write rewrites
  affected files at once, and merge-on-read writes deletion vectors or delete files and leaves the
  rewrite to compaction.

## Invariants

- Every live row exists in exactly one place: the main store or the delta store. (Kudu states the
  same of its RowSets: they form disjoint sets of rows.)
- A scan at snapshot `S` returns exactly the table's contents at `S`, whichever merges ran before or
  during it.
- A merge does not change the table's contents: the scan result before the merge equals the result
  after it.
- After a merge, the main store is sorted, its dictionary is sorted and duplicate-free, and every
  code is below the dictionary size.
- The frozen delta is not modified while it is being merged.

## Implementation pitfalls

- **Writes during the merge.** A write that lands in the frozen delta after the merge has read it is
  lost when the old delta is discarded. Freeze first, then redirect, then merge.
- **Deletes during the merge.** A delete against the old main during the merge refers to an old
  position, and positions change in the new main. Track such deletes and roll them forward onto
  the new main before the swap; the Kudu paper describes this for flushes.
- **Positions change after a merge.** Anything keyed by main-store position (a deletion vector,
  positional deltas, cached row numbers) is invalid after the swap unless remapped.
- **Releasing old stores too early.** A scan that started before the swap still reads the old main
  and delta. Hold them with a reference count (`Arc` in Rust) and free them when the last scan ends.
- **Uniqueness checks that miss a store.** During a merge a key may be in the main, the frozen delta
  or the new delta. Checking only two of the three admits duplicates.
- **Code width growth.** A merge that adds values can make the codes one bit wider: 8 distinct
  values fit in 3 bits, but a ninth needs 4. Recompute the width; do not reuse the old one.
- **Unbounded delta growth.** If merges cannot keep up with writes, the delta, and every scan's
  cost, grows without limit. Apply backpressure to writers when the delta passes a hard limit.

## Testing

- Model-based: keep a `BTreeMap<Key, Row>` as the reference, apply random interleavings of insert,
  update, delete, merge and scan to both, and compare every scan with the reference.
- Merge equivalence: scan, merge, scan again, and assert equal results; repeat with an empty delta,
  a delta of only deletes, and a delta that updates the same row many times.
- Concurrency: run merges in a background thread while writers and readers run; each reader checks
  that its result matches the reference at its snapshot.
- Edge cases: deleting a key and inserting it again before a merge, an update that changes the sort
  key, a merge that grows the dictionary across a power of two, and a crash between the freeze and
  the swap if the delta is backed by a log.

## Further reading

- [Fast Updates on Read-Optimized Databases Using Multi-Core
  CPUs](https://www.vldb.org/pvldb/vol5/p061_jenskrueger_vldb2012.pdf), Krueger et al., 2011. The
  delta merge of an in-memory column store.
- [Kudu: Storage for Fast Analytics on Fast Data](https://kudu.apache.org/kudu.pdf), Lipcon et al.,
  2015.
- [C-Store: A Column-oriented
  DBMS](https://www.vldb.org/archives/website/2005/program/paper/thu/p553-stonebraker.pdf),
  Stonebraker et al., 2005. The write-optimised and read-optimised stores.

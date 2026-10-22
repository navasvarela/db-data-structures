# Sparse primary index

> Builds on [columnar segments](../analytical-storage/columnar-segments.md).

The rows of a table are kept sorted by a key, and the index stores one entry ("mark") per granule of
rows rather than one per row. A query binary-searches the marks to find the granules that can hold
its key range, then scans only those.

- **Used by:** ClickHouse MergeTree primary keys and mark files (8,192 rows per granule by default),
  Vertica projections.
- **Cost:** small enough to stay in memory; a point lookup reads a whole granule.
- **Notes:** the same idea as an [SSTable](../lsm-trees/sstable.md)'s sparse index, applied to
  columns. C-Store (Stonebraker et al., 2005) keeps the same columns in several sort orders
  ("projections") so more queries can use one.

## How it works

### Layout

A part (an immutable [columnar segment](columnar-segments.md)) is sorted by its key, a tuple of one
or more columns, and cut into granules of a fixed number of rows. The primary index stores the key
of the first row of every granule. Each column additionally has a mark file that says where each
granule starts in that column's compressed data.

<figure class="diagram">
{{#include figures/sparse-primary-index-granules.svg}}
<figcaption>Rows sorted by <code>(user_id, ts)</code>, with 4 rows per granule here (8,192 in ClickHouse by default). The primary index holds one mark per granule, the key of its first row. The rows with <code>user_id = 17</code> (blue) span three granules.</figcaption>
</figure>

The index has one entry per granule instead of one per row, so it is small enough to keep in
memory: ClickHouse loads `primary.idx` entirely. Marks need two offsets because compression
blocks and granules do not line up: one compressed block can hold several granules, and a reader
has to decompress the block before it can find the granule inside it.

<figure class="diagram">
{{#include figures/sparse-primary-index-marks.svg}}
<figcaption>The mark file of the <code>user_id</code> column, a 4-byte integer, so each granule is 16 bytes once decompressed. Mark 2 is <code>(27, 0)</code>: decompress the block that starts at byte 27 of <code>user_id.bin</code>, and granule 2 starts at byte 0 of the result.</figcaption>
</figure>

Duplicates matter. Rows 3 to 8 share `user_id = 17`, so that value spans three granules. A granule
`i` holds keys in the closed range `[mark i, mark i+1]`, inclusive at both ends, and the search
below has to account for that.

### Build

The index is written together with the part:

1. Sort the rows by the key. ClickHouse writes each insert as a new sorted part; merges of parts
   produce sorted output from sorted inputs (see [K-way merge
   heap](../lsm-trees/k-way-merge-heap.md)).
2. Cut the sorted rows into granules of `N` rows. The last granule may be shorter.
3. For each granule, append the key of its first row to the index.
4. For each column, while compressing its values, record for each granule the offset of the
   compressed block where the granule starts and the granule's offset inside that block once
   decompressed.
5. Write the index and the mark files beside the column files.

### Look up a key range

To find the granules that may hold keys in `[lo, hi]`, where the bounds are on a prefix of the key:

1. Count the marks strictly less than `lo`; call it `a`. The first candidate granule is
   `a - 1`, or 0 when `a` is 0. Granule `a - 1` starts below `lo` and may run into it, even when
   mark `a` equals `lo`, because of duplicates.
2. Count the marks less than or equal to `hi`; call it `b`. If `b` is 0, every granule starts above
   `hi` and nothing matches. Otherwise the last candidate granule is `b - 1`.
3. For each needed column, look up the marks of the candidate granules, read and decompress the
   blocks they point to, and skip to the granule offsets.
4. Filter the rows of the candidate granules with the exact predicate.

<figure class="diagram">
{{#include figures/sparse-primary-index-search.svg}}
<figcaption>Looking up <code>user_id = 17</code> in the marks above. One mark is below <code>lo</code>, so the scan starts at granule 0, which ends with a 17 although its mark is 10. Three marks are at most <code>hi</code>, so it ends at granule 2. Granule 3 is never read.</figcaption>
</figure>

Both counts are binary searches; `slice::partition_point` does them without hand-written index
arithmetic. A point lookup reads at least one whole granule, which is the price of a sparse index.

### Predicates on later key columns

A filter on only the second key column, such as `ts = 10:30`, cannot binary-search the marks,
because `ts` is sorted only within runs of equal `user_id`. ClickHouse uses what its documentation
calls a generic exclusion search: it checks ranges of marks, excludes a range when the key values at
its ends prove no row inside can match, and splits ranges that might. If the first column has few
distinct values, long runs share one `user_id` and many ranges are excluded. If it has many, almost
nothing is. This is why the key column order should usually go from low to high cardinality.

### Variants

- **Adaptive granularity.** ClickHouse can also end a granule when it reaches a byte size
  (`index_granularity_bytes`, 10 MB by default), so granules of wide rows hold fewer rows. Mark
  files then store each granule's row count, and the index gets a final mark holding the last row's
  key.
- **Several sort orders.** C-Store and Vertica keep the same columns in several projections, each
  sorted differently, and pick the projection whose order fits the query.
- **SSTable index blocks.** An [SSTable](../lsm-trees/sstable.md) stores one index entry per data
  block, which is the same idea with blocks of key-value pairs instead of granules of columns.

## Invariants

- The marks are in non-decreasing key order (equal neighbours are allowed).
- Mark `i` equals the key of the first row of granule `i`.
- The number of marks equals the number of granules (plus one when a final mark is written).
- With fixed granularity, granule `i` starts at row `i · N`; with adaptive granularity, the sum of
  the stored row counts equals the part's row count.
- For every column, mark `i` points to the first value of row-granule `i` in that column.
- A range lookup returns every row a full scan would return for the same range.

## Implementation pitfalls

- **Duplicates across a granule boundary.** Starting the scan at the first mark equal to `lo` misses
  rows with key `lo` at the end of the previous granule. Start one granule earlier, as in step 1.
- **The last granule.** Without a final mark, the last granule has no upper bound; treat it as
  extending to positive infinity.
- **Composite keys.** Compare tuples lexicographically, column by column, with each column's own
  order. A range on the first column alone becomes a range on the tuple by padding: `user_id = 17`
  is the tuple range from `(17, smallest)` to `(17, largest)`.
- **Row number from a mark.** With adaptive granularity, granule `i` no longer starts at `i · N`.
  Keep cumulative row counts per mark, or row numbers (and anything keyed by them, such as a
  [deletion vector](deletion-vector.md)) point at the wrong rows.
- **Comparing encoded keys.** Comparing keys as raw bytes is only correct for an order-preserving
  encoding. Little-endian integers, signed integers in two's complement and floats do not sort by
  their bytes. Kudu encodes primary keys with an order-preserving encoding so that `memcmp`
  comparison is correct; otherwise compare decoded values.
- **Index arithmetic.** `a - 1` underflows when `a` is 0. In this crate clippy denies unchecked
  arithmetic; `checked_sub(1)` or `saturating_sub(1)` states the intent.

## Testing

- Against a full scan: for random sorted data, random granule sizes and random ranges, the rows
  found through the index equal the rows found by scanning everything.
- Heavy duplicates: data where one key fills several granules, with ranges that start and end
  exactly on that key, and on marks in general.
- Selectivity: on data with unique keys, a point lookup selects one granule, or two when the key
  is exactly a mark (the search cannot know the previous granule does not end with it). Assert
  this, so a bug that silently reads more granules is caught.
- Edge cases: a part with one granule, a granule size of 1, a row count that is a multiple of the
  granule size and one more, ranges entirely below the first key and above the last key, and a
  part in which every row has the same key.

## Further reading

- [A practical introduction to primary indexes in
  ClickHouse](https://clickhouse.com/docs/guides/best-practices/sparse-primary-indexes). Granules,
  mark files and how a query uses them, with worked examples.
- [C-Store: A Column-oriented
  DBMS](https://www.vldb.org/archives/website/2005/program/paper/thu/p553-stonebraker.pdf),
  Stonebraker et al., 2005. Projections in several sort orders.

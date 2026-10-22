# Zone maps

> Builds on [columnar segments](../analytical-storage/columnar-segments.md) or [heap
> file](../storage-primitives/heap-file-and-free-space-map.md).

Small per-block summaries, typically the minimum and maximum of each column. A query skips every
block whose range cannot match its predicate.

- **Used by:** PostgreSQL BRIN indexes, ClickHouse `minmax` skip indexes, Parquet column statistics,
  Snowflake micro-partition metadata.
- **Cost:** tiny; useful only when data is physically clustered by the column.

## How it works

### Layout

A zone is any contiguous unit of storage: a page, a [columnar segment](columnar-segments.md)'s row
group, a whole file, or a range of heap pages. For each zone and each indexed column, the zone map
keeps a few numbers:

```text
zone   rows            column "price"
                       min     max     null_count   nan_count   (row_count)
0      0..8191         3.50    90.00   0            0           8192
1      8192..16383     85.00   170.25  12           0           8192
2      16384..24575    —       —       8192         0           8192   ← all NULL: no bounds
3      24576..25000    0.99    12.00   0            3           425
```

Zone maps nest. Parquet keeps them per page (in its optional column index) and per column chunk
(in the footer); a table format keeps them per file in its [manifest](table-snapshot-manifest.md).
A query checks the coarsest level first and descends only into zones that survive.

### Build

While writing a zone:

1. For each value, if it is NULL, increment `null_count`. If it is a float NaN, increment
   `nan_count`. Otherwise fold it into `min` and `max` with the column's comparison order.
2. When the zone closes, store `min` and `max` only if at least one value was folded into them.
   Store the counts always.
3. If values are long strings, store a truncated lower bound and a rounded-up upper bound instead
   (see the pitfalls below), and record that the bounds are not exact.

### Prune

For each zone, evaluate the predicate against the summary and get one of three answers: no row can
match (skip the zone), every row matches (return the zone without evaluating the predicate), or
some rows may match (scan it). For a single column `x` and a constant `c`:

| Predicate | Skip the zone when | Every row matches when |
| --- | --- | --- |
| `x = c` | `c < min` or `c > max` | `min = max = c` and `null_count = 0` |
| `x < c` | `min >= c` | `max < c` and `null_count = 0` |
| `x > c` | `max <= c` | `min > c` and `null_count = 0` |
| `x BETWEEN a AND b` | `max < a` or `min > b` | `a <= min` and `max <= b` and `null_count = 0` |
| `x IN (c₁, …)` | every `cᵢ` is outside `[min, max]` | rarely worth computing |
| `x IS NULL` | `null_count = 0` | `null_count = row_count` |
| `x IS NOT NULL` | `null_count = row_count` | `null_count = 0` |

A zone with no bounds (all NULL) is skipped by every comparison, since a comparison with NULL is
never true. For floats, "every row matches" also needs `nan_count = 0`.

<figure class="diagram">
{{#include figures/zone-maps-prune.svg}}
<figcaption>The zones of the layout example under <code>price &gt; 100</code>. Zones 0 and 3 end at or below 100 and are skipped, zone 2 has no bounds and is skipped, and only zone 1 is scanned. It is a "may match" zone, not an "every row matches" one, because its minimum is below 100.</figcaption>
</figure>

Combine the answers for compound predicates:

1. `A AND B`: skip if either side says skip; all match if both say all match.
2. `A OR B`: skip if both sides say skip; all match if either says all match.
3. `NOT A`: do not flip the answer. "May match" says nothing about the complement, and a zone in
   which no row satisfies `x < c` can still have rows that do not satisfy `NOT (x < c)` either:
   the NULL rows, for which both are unknown. Push `NOT` down to the comparisons first
   (`NOT (x < c)` becomes `x >= c`, which is equivalent under SQL's three-valued logic), then
   evaluate.

### Maintain

Zone maps over immutable files are written once. Over mutable storage they have to stay correct,
which in practice means conservative:

1. An insert into a zone widens its bounds and bumps its counts. PostgreSQL's BRIN updates a
   summarised range as new tuples arrive, and leaves new ranges unsummarised until a vacuum or an
   explicit summarisation call.
2. A delete or update cannot shrink bounds cheaply, since nothing records which row held the
   minimum. Leave the bounds wide; they remain valid, only less selective. Delta Lake's
   `tightBounds` flag records exactly this state for files with [deletion
   vectors](deletion-vector.md).
3. Recompute exact bounds when the zone is next rewritten (compaction, merge or vacuum).

### Clustering decides everything

A zone map skips a zone only if the zone's range misses the predicate. On data in insertion order,
a timestamp column usually clusters well; a random UUID column never does, and every zone's range
spans nearly the whole domain. Sorting by the column, or by a [space-filling
curve](../specialised-indexes/space-filling-curves.md) over several columns, is what makes zone
maps useful.

<figure class="diagram">
{{#include figures/zone-maps-clustering.svg}}
<figcaption>The same column in four zones. Sorted, each zone covers a quarter of the domain and <code>x = 42</code> scans one zone. In an unclustered order every zone spans nearly the whole domain, so no zone can be skipped.</figcaption>
</figure>

### Variants

- **BRIN.** PostgreSQL's block range index stores a summary per range of heap pages, sized by the
  `pages_per_range` parameter. Its `minmax-multi` class keeps several intervals per range to cope
  with outliers, and its `bloom` class keeps a Bloom filter per range.
- **Skip indexes.** ClickHouse's data-skipping indexes summarise blocks of `GRANULARITY` granules
  with `minmax`, a bounded `set` of values, or a Bloom filter. The ClickHouse documentation warns
  that they skip little unless the indexed column correlates with the table's sort order.
- **Dynamic thresholds.** A [top-k heap](../query-execution/bounded-top-k-heap.md) turns its
  current k-th value into a predicate during the scan, so zone maps can skip zones that cannot beat
  it.

## Invariants

- For every zone with bounds, `min <= v <= max` for every non-NULL, non-NaN value `v` in the zone.
- A zone with at least one non-NULL, non-NaN value has bounds; a zone without one has none.
- `null_count`, when present, is exact, and `null_count <= row_count`.
- A pruned scan returns exactly the rows of a full scan with the same predicate: pruning never
  skips a zone that holds a matching row.

## Implementation pitfalls

- **NULLs in min and max.** Folding with `Option<T>`'s ordering, where `None` is smallest, makes
  the minimum of any zone with a NULL `None`, which a reader cannot compare with anything. Starting
  the fold from a sentinel such as 0 makes an all-NULL zone claim to hold zeros. Fold only non-NULL
  values, keep the bounds as an `Option<(T, T)>`, and write no bounds for a zone without values.
  Parquet's column index has a separate `null_pages` list for this reason.
- **Missing counts are not zero.** Parquet's specification says a reader must not treat an absent
  `null_count` as 0, and must assume NaNs may be present when `nan_count` is absent.
- **NaN.** `f64::partial_cmp` returns `None` for NaN, so a naive fold either panics, skips, or lets
  NaN become the min or max depending on order. Exclude NaN from the bounds and count it separately;
  Parquet and Iceberg both forbid NaN as a bound.
- **Signed zero.** `-0.0` and `+0.0` compare equal but have different bits. Parquet writes `-0.0`
  as a zero minimum and `+0.0` as a zero maximum; Iceberg requires `-0.0` to sort before `+0.0`.
  `f64::total_cmp` gives the IEEE 754 total order, but it also orders NaNs, which you have already
  excluded.
- **String truncation.** Storing a 2 KB string as a bound is wasteful, so formats allow shorter
  bounds. A prefix is a valid lower bound. A prefix is not a valid upper bound: `"abc"` is less than
  `"abcd"`. Round it up by incrementing the last byte that is not `0xFF` and dropping everything
  after it; if every byte is `0xFF`, store no upper bound. For UTF-8 columns the result must stay
  valid UTF-8, so increment a code point, not a byte. Parquet's `is_max_value_exact` and
  `is_min_value_exact` flags record whether a bound was truncated.
- **Comparison order.** Strings compare as unsigned bytes; signed and unsigned integers of the same
  width compare differently. Parquet deprecated its original `min` and `max` fields because some
  writers had used signed comparison for every type. Rust's `Ord` for `[u8]` is already unsigned
  and lexicographic.
- **Lossy statistics.** Delta Lake truncates timestamp statistics down to milliseconds, so a stored
  maximum can be up to a millisecond below the true one. A reader of such statistics has to widen
  the maximum before comparing.
- **Type coercion in the predicate.** For an integer column, `x < 2.5` must become `x <= 2` (or
  `x < 3`), not `x < 2`. Convert constants to the column's type with the rounding that keeps the
  predicate inclusive, or do not prune.

## Testing

- Pruned scan against a full scan: for random data, random zone sizes and random predicates
  (including `AND`, `OR`, `NOT`, `IN` and `IS NULL`), the pruned result must equal the full scan
  filtered row by row. This is the test that catches almost every bug on this page.
- Effectiveness: on sorted data, a point predicate should touch one or two zones; assert it, so a
  regression that silently stops pruning is caught.
- Truncation property: for random strings `s` and truncation lengths, `lower(s) <= s <= upper(s)`,
  including strings of `0xFF` bytes and multi-byte UTF-8 characters.
- Edge cases: an all-NULL zone, an all-NaN zone, a zone holding only `-0.0` and `+0.0`, a single-row
  zone, `i64::MIN` and `i64::MAX` as values and as predicate constants, and empty strings.

## Further reading

- [Small Materialized Aggregates: A Light Weight Index Structure for Data
  Warehousing](https://www.vldb.org/conf/1998/p476.pdf), Moerkotte, 1998.
- [PostgreSQL documentation: BRIN indexes](https://www.postgresql.org/docs/current/brin.html).

# Columnar batch

> Builds on [column encodings](../analytical-storage/column-encodings.md).

The unit of work in a vectorised query engine: one vector per column holding a fixed number of
values, typically in the hundreds or low thousands (DuckDB uses 2,048), each with a validity bitmap
for `NULL`s. Every operator processes a whole batch per call,
which spreads the interpretation overhead over many values. A selection vector lists the rows that
passed earlier filters, so filtering does not copy data.

- **Used by:** Apache Arrow record batches, DuckDB vectors, Velox, ClickHouse blocks, Vectorwise.
- **Cost:** batch sizes are usually chosen so the working set stays in the CPU cache. ClickHouse
  is an exception: its blocks hold tens of thousands of rows by default.
- **Notes:** Boncz, Zukowski and Nes, 2005 (MonetDB/X100).

## How it works

### Layout

A batch is a set of columns that all have the same number of rows, `n`. Each column is a *vector*:
a contiguous buffer of values of one type, plus a *validity bitmap* with one bit per row. This page
follows the [Arrow columnar format](https://arrow.apache.org/docs/format/Columnar.html), in which a
set bit means the value is present and a clear bit means it is `NULL`, and bits are numbered from
the least significant bit of each byte.

<figure class="diagram">
{{#include figures/columnar-batch-layout.svg}}
<figcaption>A batch of six rows. The validity bit of row 2 of <code>price</code> is clear, so whatever its value slot holds is never read. Row <code>i</code> of <code>name</code> is <code>data[offsets[i]..offsets[i + 1]]</code>; row 1 is the empty string, because two adjacent offsets are equal.</figcaption>
</figure>

Fixed-width values (integers, floats, dates) sit in one buffer, indexed by row. Variable-width
values (strings, byte arrays) use two buffers: one long byte buffer holding all values end to end,
and an *offsets* buffer of `n + 1` integers, where row `i` occupies the bytes from `offsets[i]` to
`offsets[i + 1]`. Row `i` of the batch is the `i`-th entry of every column.

The *selection vector* is an optional list of row indices, in increasing order, naming the rows
still alive after earlier filters. It belongs to the batch, not to a column, so one selection
vector filters every column at once without moving any value.

### Scanning a segment into a batch

1. Pick the batch size: the number of rows decoded per call. It is chosen so that the vectors an
   operator touches at once stay in the CPU cache.
2. For each column the query needs, decode the next `n` values of that column's
   [encoded segment](../analytical-storage/columnar-segments.md) into a vector, and decode or build
   its validity bitmap.
3. Leave the selection vector empty, meaning "all rows are selected".
4. Hand the batch to the next operator. The scan does not decode columns the query never reads.

### Filtering with a selection vector

To evaluate a predicate such as `price > 20`:

1. Walk the currently selected rows: every row if there is no selection vector, otherwise the rows
   it lists.
2. For each such row, test the predicate. A row whose input is `NULL` fails, since a `NULL`
   comparison is not true.
3. Write the indices of the rows that pass into a new selection vector.
4. Return the same columns with the new selection vector. No value was copied.

<figure class="diagram">
{{#include figures/columnar-batch-filter.svg}}
<figcaption>Filtering the batch above with <code>price &gt; 20</code>. Row 2 is <code>NULL</code>, so the comparison is not true and the row fails. The result is the selection vector <code>[1, 4]</code>; neither column is touched.</figcaption>
</figure>

A conjunction `a AND b` runs the second predicate only over the rows the first one selected, so the
cheapest or most selective predicate should go first.

### Computing an expression

To evaluate `price * 2` into a new column:

1. Allocate an output vector of the batch's length.
2. Run one tight loop over the input values, writing results into the output. A loop over plain
   arrays with no per-row branching is what lets the compiler use SIMD instructions.
3. Compute the output validity bitmap as the AND of the input bitmaps: the result is `NULL` where
   any input is `NULL`.
4. Engines differ on whether the loop covers every row or only the selected ones. Covering every
   row is often faster, but it must not raise errors for rows that are `NULL` or not selected (see
   the pitfalls below).

### Compacting

When a filter keeps only a small fraction of the rows, later operators waste effort skipping over
dead ones. Compaction *gathers* the selected rows of each column into a new, dense batch and drops
the selection vector. Engines usually compact when the selection drops below some threshold, or
before an operator, such as a hash table insert, that copies rows anyway.

<figure class="diagram">
{{#include figures/columnar-batch-compact.svg}}
<figcaption>Compacting after the filter: rows 1 and 4 of every column are copied into a dense batch of two rows, which needs no selection vector.</figcaption>
</figure>

### Variants

- **Selection bitmap instead of a selection vector:** one bit per row instead of a list of indices.
  It has a fixed size and is cheap to combine with AND and OR, but iterating over the selected rows
  means scanning for set bits.
- **Constant vector:** a column that holds the same value for every row, stored once. Literals and
  columns whose value is fixed for a whole segment produce these.
- **Dictionary vector:** a vector of small integer codes plus a dictionary of distinct values. An
  operator can work on the codes, for example to group or compare, without decoding the values.
- **String views:** Arrow's binary view layout stores, per row, the length and the first bytes of
  the string inline, plus a buffer index and offset for the rest of a longer string. Comparisons
  that differ in the first bytes never touch the data buffers.

## Invariants

- Every column of a batch has exactly `n` values, and its validity bitmap has at least `n` bits.
- An offsets buffer has `n + 1` entries, never decreases, and its last entry is at most the length
  of the data buffer.
- A selection vector's entries are strictly increasing and each is less than `n`.
- The value stored in a `NULL` slot is unspecified: no result may depend on it.
- Filtering never changes a column; it only produces a new selection vector.
- Evaluating an expression over a batch gives, for every selected row, the same result as
  evaluating it on that row alone.

## Implementation pitfalls

- **Reading the value behind a `NULL`.** The bytes in a `NULL` slot are whatever the producer left
  there. Code that sums all values and then subtracts nothing for `NULL`s, or compares them in a
  filter, gets wrong answers that depend on garbage. Always combine the value with its validity bit.
- **Errors from rows that should not be evaluated.** A branch-free loop over every row computes
  `a / b` even where `b` is `NULL` or the row was filtered out, and those rows may hold 0. In this
  crate, clippy denies arithmetic that can overflow or divide by zero, so the loop will use
  `checked_div` or similar; the error must then be ignored for unselected and `NULL` rows and
  reported only for live ones.
- **Bit order.** The validity bit of row `i` is bit `i % 8` of byte `i / 8`, counting from the
  least significant bit. Getting the order backwards passes tests that use all-valid or all-`NULL`
  columns and fails on mixed ones.
- **Tail bits.** When `n` is not a multiple of 8 (or of 64, if you work in words), the last byte
  has bits beyond row `n - 1`. Counting set bits over whole bytes or words counts them too, unless
  they are masked off.
- **Offset width.** Arrow's ordinary string type uses 32-bit signed offsets, which caps the data
  buffer of one vector at 2 GiB; the large variants use 64-bit offsets. Concatenating vectors, or
  building one from many long strings, has to check for overflow rather than wrap.
- **Indexing in loops.** Clippy denies `v[i]` here, so a loop over a selection vector would need
  `get` on every access. Iterating with `zip` over the input and output slices, or with
  `chunks_exact` for bitmaps, avoids both the panic path and the bounds checks.
- **Selection vectors that outlive their batch.** A selection vector holds indices into one
  particular batch. Reusing the buffers of a batch for the next one, while a downstream operator
  still holds its selection vector, silently selects different rows.
- **Batch size extremes.** A batch of one row is a correct but slow row-at-a-time engine, and a
  batch of millions spills the working set out of cache. Make the size a parameter so tests can set
  it to small and awkward values.

## Testing

- Reference model: a row-at-a-time evaluator over a `Vec` of rows, with `Option` for `NULL`. For
  every filter and expression, run both and compare the selected rows and their values.
- Property tests: generate random columns with random `NULL` positions and random selection
  vectors, and check the invariants above after every operator.
- Garbage in `NULL` slots: fill the value of every `NULL` slot with a random or extreme value
  (including 0 and `i64::MIN`) and assert results do not change.
- Batch boundaries: run the same query with batch sizes 1, 7, 8, 9, 63, 64, 65 and a large one, and
  assert identical results, so rows near byte, word and batch edges are all covered.
- Edge cases: an empty batch, a batch where every row is `NULL`, a filter that selects nothing, a
  filter that selects everything, and empty strings (two equal adjacent offsets).

## Further reading

- [MonetDB/X100: Hyper-Pipelining Query Execution](https://www.cidrdb.org/cidr2005/papers/P19.pdf),
  Boncz, Zukowski and Nes, 2005.
- [Apache Arrow columnar format](https://arrow.apache.org/docs/format/Columnar.html). Validity
  bitmaps, offsets buffers and record batches, specified byte by byte.
- [Everything You Always Wanted to Know About Compiled and Vectorized Queries But Were Afraid to
  Ask](https://www.vldb.org/pvldb/vol11/p2209-kersten.pdf), Kersten et al., 2018. Vectorised and
  compiled execution compared.

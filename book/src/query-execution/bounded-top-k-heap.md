# Bounded top-k heap

A binary heap holding the best `k` rows seen so far, for `ORDER BY ... LIMIT k`. Any row worse than
the root is discarded at once.

- **Used by:** PostgreSQL's top-N heapsort, top-N operators in DuckDB and ClickHouse.
- **Cost:** `O(n log k)` time and `O(k)` memory, instead of sorting all `n` rows.
- **Notes:** the root works as a filter that tightens during the query: rows, and whole blocks
  through [zone maps](../analytical-storage/zone-maps.md), that cannot beat it can be skipped in the
  scan.

## How it works

### Layout

The heap is an array of at most `k` entries, arranged as an implicit binary tree: the children of
position `i` are at `2i + 1` and `2i + 2`. Each entry holds the sort key of a row and either the row
itself or a reference to it.

The heap is ordered so that the root is the *worst* row kept so far, the one that would be the
first to drop out of the result. For `ORDER BY x ASC LIMIT k` that is the row with the largest
`x`, so the heap is a max-heap on `x`, even though the query asks for the smallest values.

<figure class="diagram">
{{#include figures/bounded-top-k-heap-layout.svg}}
<figcaption>The heap for <code>ORDER BY x ASC LIMIT 5</code> after reading 9, 2, 14, 7, 3, 11, 5. The root, 9, is the worst of the best five and the current threshold. The children of array position <code>i</code> are at <code>2i + 1</code> and <code>2i + 2</code>.</figcaption>
</figure>

### Push

1. If the heap holds fewer than `k` entries, append the row at the end of the array and sift it up:
   while it is worse than its parent, swap them.
2. Otherwise, compare the row with the root. If it is not strictly better, discard it. This single
   comparison is the common case once the heap has filled, and it is why most rows cost `O(1)`.
3. If it is better, overwrite the root with it and sift it down: while it is better than its worse
   child, swap them.

<figure class="diagram">
{{#include figures/bounded-top-k-heap-push.svg}}
<figcaption>The last pushes of the example. Once five rows are in, 11 beats the root 14 and replaces it; 11 is worse than both its children, so it stays. 5 replaces 11 and is better than its worse child, 9, so they swap. A further row with <code>x = 10</code> is not better than the root and costs a single comparison.</figcaption>
</figure>

### Finish

When the input is exhausted, the heap holds the `k` best rows in heap order, not sorted order.
Popping the root repeatedly yields them from worst to best; filling an output array from the back
gives the final order. Alternatively, sort the `k` entries, which costs `O(k log k)`.

### Pruning the scan

Once the heap is full, its root is a threshold: no row worse than it can enter the result.

1. Publish the root's key to the scan each time it changes.
2. Before decoding a block, compare the threshold with the block's minimum and maximum from its
   [zone map](../analytical-storage/zone-maps.md). For `ASC`, if the block's minimum is not better
   than the threshold, skip the whole block.
3. Inside a block, a filter `x < threshold` can be applied before the heap, in vectorised form.

The threshold only becomes valid when the heap holds `k` rows; before that, nothing may be skipped.

<figure class="diagram">
{{#include figures/bounded-top-k-heap-prune.svg}}
<figcaption>Pruning with the full heap above. Every value in block 2 is at least 12, so none can beat the root 9, and the block is never decoded. Blocks 1 and 3 may hold rows below 9 and are read.</figcaption>
</figure>

### Parallel top-k

1. Each thread keeps its own bounded heap over its share of the input.
2. At the end, push every entry of every thread's heap into one heap of size `k`.

This is correct because the best `k` rows of the whole input are among the best `k` of each part.
Threads may also share their thresholds, so a thread can prune with the best threshold any thread
has reached.

### Variants

- **`OFFSET`:** `LIMIT k OFFSET o` needs the best `k + o` rows, then drops the first `o`.
- **`WITH TIES`:** rows equal to the `k`-th row must also be returned, so the structure has to keep
  every row tied with the root, not only `k` entries.
- **Switching from a buffer to a heap:** PostgreSQL's sort collects rows into an array first and
  converts it to a bounded heap once it holds more than twice the bound, a threshold its source
  describes as a heuristic for where a heap becomes cheaper than a quicksort.
- **Selection instead of a heap:** collect rows into a buffer of `2k`; when it is full, partition
  it around the `k`-th element (with `select_nth_unstable` in Rust) and drop the worse half. This
  trades the heap's per-row `O(log k)` for expected amortised constant work per row, done in larger
  batches.
- **Late materialisation:** keep only the sort key and a row identifier in the heap, and fetch the
  other columns for the `k` winners at the end. This keeps the heap small when rows are wide.

## Invariants

- The heap never holds more than `k` entries, and after `n` pushes it holds `min(n, k)`.
- Heap order: no entry is worse than its parent, so the root is the worst entry.
- Every entry in the heap is at least as good as every row discarded so far.
- The root's key, once the heap is full, only ever gets better (the threshold never loosens).
- The final output is sorted and equals, by key, the first `k` rows of a full sort of the input.

## Implementation pitfalls

- **Heap direction.** To keep the smallest `k`, use a max-heap; to keep the largest `k`, a min-heap.
  Rust's `BinaryHeap` is a max-heap, so `ORDER BY x DESC LIMIT k` needs `Reverse`. Getting this
  backwards returns the worst `k` rows, and a test with sorted input can still pass.
- **Composite and mixed-direction keys.** `ORDER BY a ASC, b DESC NULLS LAST` needs one comparator
  that compares `a`, then `b` reversed, with the `NULL` placement applied per column. Build it from
  per-column comparisons chained with `Ordering::then_with`, and test every combination.
- **`NULL` ordering.** SQL lets the query choose `NULLS FIRST` or `NULLS LAST`, and systems differ
  on the default. Wrapping values in `Option` and relying on its `Ord` puts `None` first, which is
  only right for one of the cases.
- **Floats.** `f64` is not `Ord` because of `NaN`. Use `f64::total_cmp`, and decide where `NaN` goes
  relative to `NULL`. An inconsistent comparator does not only give wrong answers: since Rust 1.81
  the standard library's sorts may panic when the ordering is not total.
- **Ties at the boundary.** Without `WITH TIES`, which of several rows equal to the `k`-th one is
  returned is unspecified. A test that compares whole rows against a reference sort fails at random;
  compare the sorted keys instead.
- **`k = 0` and huge `k`.** `LIMIT 0` must return nothing without touching the heap. A `LIMIT`
  larger than the input must not pre-allocate `k` entries; grow the array as rows arrive.
- **Overflow of `k + o`.** With an `OFFSET`, the bound is `k + o`, which can overflow a `usize` for
  absurd inputs. Clippy denies the plain `+` here; use `checked_add` and treat overflow as "no
  bound".
- **Pruning too early.** Using the root as a threshold before the heap holds `k` rows skips rows
  that belong in the result.
- **Index arithmetic.** `2i + 1` can overflow in principle, and clippy denies both the plain
  arithmetic and `v[i]`. Use `checked_mul`/`checked_add` and `get`/`swap` on the array.

## Testing

- Reference model: sort the whole input with the same comparator and take the first `k`. Compare the
  keys of the two outputs, position by position.
- Property tests: random inputs with many duplicates, `NULL`s, `NaN`s and all sort directions, and
  random `k` from 0 to beyond the input length.
- Input orders: already sorted, reverse sorted and all equal. Sorted input in the "wrong" direction
  makes every row replace the root, which exercises sift-down hardest.
- Parallel merge: split the input at random points into several heaps, merge them, and compare with
  the single-heap result.
- Pruning: run the same query with and without zone-map pruning and assert identical output, and
  count skipped blocks on input arranged so that pruning must happen.

## Further reading

- [A Survey of Top-k Query Processing Techniques in Relational Database
  Systems](https://doi.org/10.1145/1391729.1391730), Ilyas, Beskales and Soliman, 2008.

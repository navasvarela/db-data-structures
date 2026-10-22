# Count-min sketch

A grid of counters with one hash function per row. It estimates how often an item occurred, never
underestimating.

- **Used by:** TinyLFU cache admission, heavy-hitter detection in stream processors.
- **Notes:** Cormode and Muthukrishnan, 2005.

## How it works

### Layout

The sketch is a `d × w` array of counters, all zero at first, and `d` hash functions, one per row,
each mapping an item to a column in `0..w`.

<figure class="diagram">
{{#include figures/count-min-sketch-query.svg}}
<figcaption>A sketch with <code>d = 3</code> and <code>w = 7</code>. Each row's hash sends <code>x</code> to one counter, and the smallest of the three, 12, is the estimate.</figcaption>
</figure>

Each counter holds the total count of every item that hashes to it. Collisions only ever add
other items' counts, so every counter on an item's path is at least its true count, and the
smallest of them is the best estimate.

### Sizing and error bound

Let `N` be the sum of all counts inserted, `ε` the tolerated error as a fraction of `N`, and `δ`
the probability of exceeding it. Choose

```text
w = ceil(e / ε)          d = ceil(ln(1 / δ))
```

Then for every item, with probability at least `1 - δ`,

```text
true(x) ≤ estimate(x) ≤ true(x) + ε · N
```

For `ε = 0.1%` and `δ = 1%` that is `w = 2,719` and `d = 5`: about 13,600 counters, whatever the
number of distinct items.

Why it holds: in one row, the other items' counts that collide with `x` add up, in expectation, to
at most `N / w = ε · N / e`. By Markov's inequality, that row overshoots by more than `ε · N` with
probability at most `1 / e`. The rows use independent hashes, so all `d` rows overshoot with
probability at most `e^(-d) ≤ δ`. The hash functions need only be pairwise independent for this
argument.

The bound is additive in `N`, which makes the sketch accurate for frequent items and useless for
rare ones: with `ε = 0.1%`, an item seen 10 times in a stream of ten million is only estimated
to within 10,000.

### Update

1. For each row `i`, compute the column `j = h_i(x)`.
2. Add the count `c` (usually 1) to `C[i][j]`.

### Point query

1. For each row `i`, read `C[i][h_i(x)]`.
2. Return the minimum.

### Merge

Two sketches with the same `w`, `d` and hash functions merge by adding them cell by cell. The
sketch is linear, so the result is identical to the sketch of the combined stream.

### Deletions

Subtracting `c` from the same `d` cells removes occurrences. The minimum stays an overestimate as
long as no item's true count goes negative. If counts can go negative, take the median of the `d`
row values instead of the minimum, which loses the one-sided guarantee.

### Variants

- **Conservative update** (Estan and Varghese, 2002): to add `c`, first compute the current
  estimate `m`, then raise each of the `d` cells to `max(cell, m + c)` instead of adding `c` to
  all of them. Cells grow less, so estimates are never worse and are often much better on skewed
  data. It cannot support deletions, and a merge by addition still overestimates but is no longer
  identical to the sketch of the combined stream.
- **Heavy hitters:** keep the sketch plus a
  [bounded top-k heap](../query-execution/bounded-top-k-heap.md) of the items with the largest
  estimates, updating the heap after each insert. This finds the items above a given fraction of
  `N` in one pass.
- **Count sketch** (Charikar, Chen and Farach-Colton, 2002): each row also hashes the item to a
  sign `±1`, and the estimate is the median of `sign · counter`. Its error depends on the square
  root of the sum of squared counts, which is never more than `N` and much less when the count is
  spread over many items, but it can underestimate.
- **Aging:** TinyLFU uses small saturating counters and periodically halves all of them, so the
  sketch reflects recent popularity rather than all-time counts.

<figure class="diagram">
{{#include figures/count-min-sketch-conservative.svg}}
<figcaption>Conservative update, the first variant above, adding one occurrence of <code>x</code>. The plain update adds 1 to all three. The conservative update only raises counters below the new estimate, 13, so rows 0 and 2 keep their values.</figcaption>
</figure>

## Invariants

- Without deletions, every row sums to `N`, the total count inserted.
- For every item, `estimate(x) ≥ true(x)` (without deletions, or with deletions that never make a
  true count negative).
- `merge(A, B)` equals the sketch of the concatenated streams, cell for cell, for the plain update
  rule.
- With conservative update, every cell is at most the corresponding cell of a plain sketch with
  the same hashes fed the same stream.
- `w`, `d`, the hash family and its seeds are fixed for the sketch's lifetime and serialised with
  it.

## Implementation pitfalls

- **Rows that are not independent.** Using the same hash for every row makes all rows identical,
  so `d` rows give the accuracy of one. Derive per-row hashes from a seeded family, for example
  `h_i(x) = h1(x) + i · h2(x)` from one 64-bit hash, as Bloom filters do, and reduce to `0..w`
  with a multiply-shift.
- **Swapping `w` and `d`.** Width controls the error size and depth the failure probability. A
  sketch of 5 columns and 2,719 rows is the same size and almost useless.
- **Counter overflow.** With `u32` counters and large weights, cells wrap around and a frequent
  item suddenly looks rare. Use `u64`, or saturate with `saturating_add`; this crate's clippy
  settings deny plain overflowing arithmetic anyway.
- **Conservative update with deletions or merges.** Decrementing a conservatively updated sketch
  can produce underestimates. Pick one rule per sketch and record it in the serialised form.
- **Unstable hashing.** Rust's `DefaultHasher` and `RandomState` differ between processes, so
  sketches built in different processes cannot be merged. Use a specified hash with stored seeds.
- **Flat storage indexing.** Store the grid as one `Vec` in row-major order and compute
  `i · w + j` with checked arithmetic, reading with `get`, since clippy here denies `v[i]`.
- **Expecting relative accuracy.** Do not use the estimate of a rare item to decide anything
  without checking it against `ε · N`.

## Testing

- Exact comparison: count a Zipf-distributed stream exactly in a `HashMap` and assert
  `estimate(x) ≥ true(x)` for every item.
- Error bound: measure the fraction of items whose overestimate exceeds `ε · N`; over many fixed
  seeds it should be at most about `δ`.
- Row sums: after any sequence of inserts, every row sums to `N`.
- Merge equivalence: sketch two halves of a stream and the whole stream with the same seeds;
  the merged sketch must equal the whole-stream sketch exactly.
- Deletions: insert, then delete some items, and check the estimates of deleted items fall back to
  their remaining counts plus collisions.
- Conservative update: with the same seeds and stream, every conservative estimate is at least the
  true count and at most the plain estimate.
- Edge cases: an empty sketch (every estimate is 0), `w = 1` or `d = 1`, and weights near the
  counter's maximum.

## Further reading

- [An Improved Data Stream Summary: The Count-Min Sketch and its
  Applications](https://doi.org/10.1016/j.jalgor.2003.12.001), Cormode and Muthukrishnan, 2005.
- [Count-Min Sketch](https://florian.github.io/count-min-sketch/), Hartmann. A short illustrated
  introduction.

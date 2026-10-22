# KLL sketch

A quantile sketch built from a stack of compactors: each level holds a buffer, and when a buffer
fills, it is sorted and every other item is promoted to the next level with doubled weight. It has a
proven error bound on rank, where t-digest's accuracy is empirical.

- **Used by:** Apache DataSketches, Apache Druid.
- **Cost:** near-optimal space for a given rank error; mergeable.
- **Notes:** Karnin, Lang and Liberty, 2016.

## How it works

### Rank error

The **rank** of a value `x` is the number of inserted items less than or equal to `x`; its
**normalised rank** is that number divided by `n`. A quantile query asks the inverse: which item
has normalised rank `q`. KLL guarantees that, for any single query, with probability at least
`1 - δ` the rank it reports is within `ε · n` of the true rank. Covering every possible query at
once needs a smaller `δ`, which costs a little more space. The error is additive in rank, the same
for p50 and p99, unlike [t-digest](t-digest.md), which is most precise at the tails without a
guarantee.

### The compactor

A **compactor** is a buffer of items that all carry the same weight `w`. Compacting it:

1. Sort the items.
2. Flip a fair coin, and keep either the items at even positions or the items at odd positions.
3. Give each kept item weight `2w` and discard the rest.

<figure class="diagram">
{{#include figures/kll-sketch-compact.svg}}
<figcaption>Compacting six items of weight <code>w</code>. The coin picks the odd positions, so 2, 9 and 20 survive with weight <code>2w</code>; the total stays <code>6w</code>. Three items are at most 10, an odd number, so the rank of 10 moves from <code>3w</code> to <code>4w</code>. Had the coin picked the even positions, it would have moved to <code>2w</code>.</figcaption>
</figure>

Total weight is preserved. For any query value `x`, a compaction leaves its rank unchanged if an
even number of the buffer's items are `≤ x`, and otherwise moves it by `+w` or `-w` with equal
probability. Each compaction therefore adds an independent error with mean zero, and the errors
cancel rather than accumulate, which is what the analysis exploits.

### Layout

A KLL sketch is a stack of compactors. Level `h` holds items of weight `2^h`; new items enter
level 0 with weight 1. Each level's capacity shrinks geometrically going down from the top:

```text
k_h = max(m, ceil(k · c^(H - 1 - h)))        for H levels, h = 0 (bottom) to H - 1 (top)
```

<figure class="diagram">
{{#include figures/kll-sketch-levels.svg}}
<figcaption>A sketch with <code>k = 200</code>, <code>c = 2/3</code> and <code>H = 4</code>, drawn with bar length proportional to capacity and the filled part showing the items held. Every level above level 0 is kept sorted. Level 0 is the lowest full level, so the next compaction moves half of its items up to level 1 with weight 2.</figcaption>
</figure>

`k` sets the accuracy, `c` the decay (it must lie between 1/2 and 1; the paper suggests 2/3), and
`m` is a minimum capacity, 8 in DataSketches. Because capacities decay geometrically, the whole
sketch holds about `k / (1 - c) = 3k` items plus a few small levels, however long the stream.

### Insert

1. Append the item to level 0 and add 1 to `n`. Update the minimum and maximum.
2. If the sketch is over capacity, find the lowest level that is full and compact it into the
   level above:
   1. Sort the level if it is not sorted.
   2. If it holds an odd number of items, set one aside; it stays on this level.
   3. Compact the rest as above, and merge the kept items into the level above, keeping that level
      sorted. If this was the top level, create a new top level, which changes `H` and therefore
      every level's capacity.
3. Repeat step 2 while some level is still over capacity.

DataSketches describes its scheme as lazy: rather than compacting every level the moment it
fills, it compacts only when the sketch as a whole runs out of room, starting from the lowest
full level.

### Rank and quantile queries

1. Rank of `x`: sum `2^h` for every item `≤ x` on every level `h`, and divide by `n`.
2. Quantile `q`: collect every item with its weight, sort by item, take cumulative weights, and
   return the first item whose cumulative weight reaches `q · n`. Build this sorted view once and
   reuse it until the next insert.

Every answer is a value that was actually inserted.

### Merge

1. Add the two `n` values, and take the minimum of the minimums and the maximum of the maximums.
2. For each height, concatenate the two sketches' levels, keeping the taller sketch's height.
3. Compact from the bottom up until every level is within capacity.

The error guarantee holds for any sequence of merges, which is the property t-digest lacks.

### Error and size

The paper shows the compactor stack with capacities `c^(H-h)` reaches rank error `ε` with
probability `1 - δ` in `O((1/ε) · √(log(1/δ)))` space while staying mergeable. Its final,
non-mergeable variant replaces the top compactors with a GK sketch to reach
`O((1/ε) · log log(1/δ))`, which the paper proves optimal. In practice the error is roughly
proportional to `1 / k`: DataSketches reports, for its default `k = 200`, a normalised rank error
of about 1.33% for single queries and 1.65% for PMF and histogram queries, and about 2.61% and
3.18% for `k = 100`.

### Variants

- **Sampler at the bottom:** a chain of capacity-2 compactors is equivalent to keeping one random
  item out of every `2^j`, so the lowest levels can be replaced by a sampler in constant space.

## Invariants

- `Σ_h 2^h · |level h| = n`: compaction and merge preserve total weight exactly.
- Every level above level 0 is sorted.
- After an insert or merge returns, the sketch is within its capacity.
- `min ≤ item ≤ max` for every stored item, `rank(max) = 1` and `quantile(0) = min`.
- Rank estimates are non-decreasing in `x`, and quantile estimates are non-decreasing in `q`.
- While `n` is below the level-0 capacity, no compaction has happened and every answer is exact.

## Implementation pitfalls

- **Deterministic compaction.** Always keeping the even positions makes every compaction err in
  the same direction for a given query, so errors add up instead of cancelling. Use a random bit
  per compaction, from a seeded generator passed in, so tests are reproducible.
- **Odd-sized levels.** Compacting an odd number of items either loses or invents weight. Set one
  item aside first, and check the weight invariant after every compaction.
- **Stale capacities.** Capacities depend on `H - 1 - h`. When a new top level appears, every lower
  level's capacity shrinks; compute capacities from the current `H` rather than storing them.
- **Weights as shifts.** `1 << h` overflows past `h = 63`. Use `u64` with `checked_shl`, since
  clippy here denies overflowing arithmetic, and keep `n` as a `u64` too.
- **Rank conventions.** Decide whether rank counts items `≤ x` or `< x`, and use the same rule in
  rank, quantile and tests. Ties and duplicates expose the difference.
- **Float items.** `f64` is not `Ord`. Reject NaN on insert and compare with `f64::total_cmp`.
- **Merging levels unsorted.** Concatenating two sorted levels gives an unsorted level. Merge them
  as sorted runs, or mark the level unsorted and sort before the next compaction.
- **Indexing.** Choose even or odd positions with `iter().skip(offset).step_by(2)` instead of
  `v[i]`, which clippy here denies.

## Testing

- Exact comparison: insert a million values from several distributions and orders (random,
  sorted, reverse, many duplicates), sort a copy, and compute the maximum rank error over many
  query points. DataSketches' published 1.33% for `k = 200` is a guide: check that the error stays
  below, say, 2% of `n` over 100 fixed seeds, allowing a small number of failures.
- Weight conservation: after every insert, compaction and merge, assert `Σ 2^h · |level h| = n`.
- Unbiasedness: average the signed rank error at a fixed `x` over many seeds; it should be close
  to zero.
- Merge: split a stream into 2, 10 and 100 parts, sketch and merge them in random orders, and check
  the merged sketch meets the same error tolerance as the single-pass sketch.
- Exact mode: for `n` below the level-0 capacity, quantiles must equal the exact ones.
- Determinism: the same seed and input give an identical serialised sketch.
- Edge cases: an empty sketch, one item, all items equal, `k` at its minimum, and NaN rejected.

## Further reading

- [Optimal Quantile Approximation in Streams](https://arxiv.org/abs/1603.05346), Karnin, Lang and
  Liberty, 2016.
- [KLL sketch](https://datasketches.apache.org/docs/KLL/KLLSketch.html), Apache DataSketches
  documentation.

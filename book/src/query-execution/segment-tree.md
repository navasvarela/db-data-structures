# Segment tree

A binary tree over an array in which each node stores the aggregate of its range, so any range
aggregate is the combination of `O(log n)` nodes. A Fenwick tree is a more compact form for
aggregates that can be subtracted, such as sums.

- **Used by:** window functions with sliding frames (`SUM(x) OVER (ROWS BETWEEN ...)`) in HyPer,
  Umbra and DuckDB; prefix sums over data cubes.
- **Cost:** `O(n)` build, `O(log n)` per range query.
- **Notes:** Leis et al., 2015 (window functions in analytical queries); Fenwick, 1994.

## How it works

### Layout

The tree needs an aggregate function `combine` that is *associative*
(`combine(combine(a, b), c) = combine(a, combine(b, c))`) and has an *identity* element `e` with
`combine(e, a) = a`. Sum with 0, min with `+∞`, max with `-∞` and count with 0 all qualify.
`combine` does not have to be commutative, as long as the code always keeps left before right.

The common implementation stores the tree in one array of size `2n`, with no pointers. The input
values are the leaves, at positions `n` to `2n - 1`; the node at position `i` has children `2i`
and `2i + 1`, and position 0 is unused. Each inner node holds `combine` of its two children.

<figure class="diagram">
{{#include figures/segment-tree-layout.svg}}
<figcaption>A sum tree over eight values. Each inner node holds the sum of its two children; in the array, node <code>i</code> has its children at <code>2i</code> and <code>2i + 1</code>, and the input occupies positions 8 to 15.</figcaption>
</figure>

### Build

1. Copy the input into positions `n` to `2n - 1`.
2. For `i` from `n - 1` down to 1, set `t[i] = combine(t[2i], t[2i + 1])`.

Each inner node is computed once from two children that are already final, so the build is
`O(n)`.

### Range query

To aggregate the half-open range `[l, r)`:

```text
l += n; r += n
left = e; right = e
while l < r:
    if l is odd:  left  = combine(left, t[l]);  l += 1
    if r is odd:  r -= 1;  right = combine(t[r], right)
    l /= 2; r /= 2
return combine(left, right)
```

1. Start both bounds at the leaf level.
2. A left bound at an odd position is a right child, so its parent covers values outside the range:
   take the node itself and step past it. The right bound is symmetric.
3. Move both bounds up one level and repeat until they meet.
4. Combine the two accumulators. Keeping `left` and `right` separate is what makes the result
   correct for a non-commutative `combine`.

At most two nodes are taken per level, so a query costs `O(log n)`.

<figure class="diagram">
{{#include figures/segment-tree-query.svg}}
<figcaption>The sum of <code>input[1..8)</code>. Each pass takes at most one node per side and then halves both bounds. Three nodes, at positions 9, 5 and 3, cover the range exactly, and their sum is 7 + 13 + 20 = 40.</figcaption>
</figure>

### Point update

1. Set the leaf: `i = n + index`, `t[i] = value`.
2. While `i > 1`: `i /= 2`, then `t[i] = combine(t[2i], t[2i + 1])`.

Window functions do not need updates, since the input is fixed once sorted, but they make the tree
useful elsewhere and are a good test of the layout.

### Evaluating a window frame

For `SUM(x) OVER (PARTITION BY p ORDER BY o ROWS BETWEEN a PRECEDING AND b FOLLOWING)`:

1. Partition the input by `p` and sort each partition by `o`.
2. Build one segment tree over the `x` values of each partition, in sorted order.
3. For the row at position `i`, compute its frame as `[i - a, i + b + 1)`, clamped to the partition.
   For a `RANGE` frame, find the bounds by binary search on `o` instead.
4. Answer the frame with one range query.

Leis et al. (2015) use this for frames whose bounds move arbitrarily from row to row, where
caching the previous frame's aggregate does not help. Their tree differs from the binary layout
above in three ways: the lowest level is the sorted input itself, so it is not copied; nodes have
a fanout of 16 rather than 2, which they found to be a good balance between space and speed; and
each level can be built in parallel, since its nodes depend only on the level below. For derived
aggregates such as `avg`, they store the sum and the count in the same tree.

### Fenwick tree

A Fenwick tree (binary indexed tree) stores, at 1-based position `i`, the aggregate of the
`lowbit(i)` values ending at `i`, where `lowbit(i) = i & (-i)` is the lowest set bit of `i`. It
answers prefix aggregates in `O(log n)` with an array of exactly `n` entries:

```text
prefix(i):  acc = e; while i > 0: acc = combine(acc, f[i]); i -= lowbit(i)
add(i, d):  while i <= n: f[i] = combine(f[i], d); i += lowbit(i)
```

A range `[l, r)` is then `prefix(r) - prefix(l)`, which needs subtraction. That is why it works for
sums and counts but not for min or max.

### Variants

- **Higher fanout:** fewer levels and less extra space, at the cost of combining up to `fanout - 1`
  siblings per level in a query.
- **Sparse table:** for min and max over a fixed array, precompute the aggregate of every range of
  length `2^j`. A query combines two overlapping ranges in `O(1)`, which is only correct for
  operations where counting a value twice does not matter. It costs `O(n log n)` space.
- **Lazy propagation:** supports adding a value to a whole range in `O(log n)` by deferring the
  update at inner nodes. Window evaluation does not need it.

## Invariants

- For every inner node `i`: `t[i] = combine(t[2i], t[2i + 1])`.
- A range query over `[l, r)` equals the left-to-right fold of `combine` over `input[l..r)`, and the
  query over an empty range returns `e`.
- Padding leaves, if any, hold `e`.
- After a point update, every ancestor of the changed leaf satisfies the first invariant again.
- For a Fenwick tree: `prefix(i)` equals the sum of the first `i` values.

## Implementation pitfalls

- **Sizing for `n` that is not a power of two.** A recursive segment tree that splits ranges in
  half needs up to `4n` slots, not `2n`, when `n` is not a power of two. The iterative `2n` layout
  above answers bottom-up queries correctly for any `n`, even with a non-commutative `combine`, but
  some nodes near the top then mix values from both ends of the array, out of order. They must never
  be read directly: in particular, `t[1]` is the in-order total only when `n` is a power of two. If
  you need the root, or a top-down descent, pad `n` up to a power of two with identity leaves.
- **Open and closed bounds.** SQL frames are inclusive (`BETWEEN 2 PRECEDING AND CURRENT ROW` has
  three rows), and the query above is half-open. Convert once, in one place.
- **Frames that leave the partition.** `i - a` underflows near the start of a partition and
  `i + b + 1` can pass its end. With clippy denying overflowing arithmetic, use `saturating_sub`
  and `min(partition_len)`, and treat a frame whose start is at or past its end as empty.
- **Empty frames.** An empty frame must yield SQL's result for no rows, which is `NULL` for `SUM`,
  `MIN` and `AVG` and 0 for `COUNT`. The identity element is not always the SQL answer.
- **Identity for min and max.** Using `i64::MAX` as the identity of `min` breaks when the data
  contains `i64::MAX` and an empty range must be told apart from a real result. Using `Option` as
  the aggregate type, with `None` as identity, avoids this.
- **Overflow in inner nodes.** With mixed signs, the sum stored in a node can overflow even though
  every query result the user asks for would fit. Decide whether to accumulate in a wider type, such
  as `i128` for `i64` input, or to report overflow; either way, use checked arithmetic.
- **Floating-point results that differ from a running sum.** The tree adds values in a different
  order from a left-to-right loop, so float results can differ in the last bits. A Fenwick tree that
  subtracts prefixes is worse: subtracting two large prefix sums loses the precision of small
  ranges.
- **Non-commutative aggregates.** Combining the right accumulator on the wrong side gives correct
  sums and wrong results for anything order-dependent, such as `first_value`, `last_value` or string
  concatenation.
- **Index arithmetic.** `2i + 1` and `n + index` need `checked_*` under this crate's lints, and
  array access needs `get`, since clippy denies `t[i]`. For a Fenwick tree on unsigned indices,
  `lowbit(i)` is `i & i.wrapping_neg()`.

## Testing

- Reference model: fold `combine` over the slice directly. For every `n` from 0 to about 40
  (covering powers of two and the values next to them), check every pair `l <= r`.
- Order sensitivity: test with a non-commutative `combine`, such as string concatenation or a pair
  `(first, last)`, so bugs that swap left and right are caught. Sums never catch them.
- Property tests: random arrays and random sequences of updates and queries, checked against the
  reference after every step.
- Window frames: compare the tree's per-row results with a naive loop over each row's frame, for
  `ROWS` and `RANGE` frames, frames that are entirely before or after the row, and frames larger
  than the partition.
- Edge cases: `n = 0`, `n = 1`, empty ranges, values at the integer limits, and integer-valued data
  when testing float sums, so exact comparison is still valid.

## Further reading

- [Efficient Processing of Window Functions in Analytical SQL
  Queries](https://www.vldb.org/pvldb/vol8/p1058-leis.pdf), Leis et al., 2015.
- [Segment tree](https://cp-algorithms.com/data_structures/segment_tree.html) and [Fenwick
  tree](https://cp-algorithms.com/data_structures/fenwick.html), cp-algorithms. Implementation-level
  tutorials.

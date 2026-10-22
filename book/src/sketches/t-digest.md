# t-digest

A sketch of a value distribution as weighted centroids, more precise near the tails. It answers
approximate quantile queries such as p99 latency, and two digests can be merged.

- **Used by:** ClickHouse `quantileTDigest`, Elasticsearch percentiles.
- **Notes:** Dunning and Ertl, 2019.

## How it works

### Layout

A t-digest summarises `n` values as a list of **centroids**, each a pair `(mean, weight)`: the
mean of a group of nearby values and how many values it absorbed. Centroids are kept sorted by
mean. The digest also keeps the exact minimum and maximum, the total weight `n`, a **compression**
parameter `δ` (typically 100 to 1,000) and an unsorted buffer of recent values.

<figure class="diagram">
{{#include figures/t-digest-centroids.svg}}
<figcaption>A digest of 150 values in eleven centroids, drawn in order of their means with area proportional to weight.</figcaption>
</figure>

Clusters are small near the extremes and large in the middle. A quantile near 0 or 1 is
therefore estimated from clusters of one or two values, which is what makes p99.9 accurate.

### Scale functions and the size bound

A **scale function** `k(q)` maps a quantile `q` in `[0, 1]` to a notional index, and grows fastest
near the ends. The paper's four:

```text
k0(q) = (δ / 2) · q
k1(q) = (δ / 2π) · asin(2q - 1)
k2(q) = δ / (4 log(n/δ) + 24) · log(q / (1 - q))
k3(q) = δ / (4 log(n/δ) + 21) · log(2q)              if q ≤ 1/2
                              · -log(2 (1 - q))      if q > 1/2
```

For a cluster holding the values between quantiles `q_left` and `q_right`, the **size bound** is

```text
k(q_right) - k(q_left) ≤ 1          (unless the cluster holds a single value)
```

Where `k` is steep, near `q = 0` and `q = 1`, the bound only admits clusters that span a tiny
range of `q`. `k0` is linear, so it gives equal-sized clusters and constant error, like a
histogram. `k1` keeps cluster sizes proportional to `√(q (1 - q))`; `k2` and `k3` keep them
proportional to `q (1 - q)` and squeeze the tails harder. With any of these, a fully merged
digest has at most `ceil(δ)` centroids. For `k2` and `k3` this holds up to about `10^60` values,
after which the count grows very slowly.

<figure class="diagram">
{{#include figures/t-digest-scale.svg}}
<figcaption>The scale function <code>k1</code> with <code>δ = 20</code>. Cutting <code>k</code> into steps of one gives ten clusters; mapped back to <code>q</code>, the outer clusters cover about 2.5% of the values each and the two middle ones about 15.5% each.</figcaption>
</figure>

### Insert

1. Append the value to the buffer with weight 1, and update the minimum and maximum.
2. If the buffer is full, run the merge pass below. A buffer of several times `δ` values amortises
   the sort; the paper reports large speed-ups up to about `10 · δ`.

### The merge pass

1. Sort the buffer together with the existing centroids by mean.
2. Let `S` be the total weight. Start a current centroid with the first item, set `q0 = 0` and
   compute `q_limit = k⁻¹(k(q0) + 1)`, the furthest quantile the current centroid may reach.
3. For each next item `x`:
   1. If `q0 + (weight(current) + weight(x)) / S ≤ q_limit`, absorb `x` into the current centroid:
      add the weights and move the mean to the weighted average.
   2. Otherwise emit the current centroid, advance `q0` by its weight over `S`, recompute
      `q_limit`, and start a new current centroid with `x`.
4. Emit the last centroid. The result replaces the centroid list and the buffer is cleared.

Because items are always absorbed from left to right, clusters near `q = 0` grow asymmetrically
and their means drift. The paper mitigates this by alternating the direction of the scan, ascending
on one pass and descending on the next.

### Quantile query

To estimate the value at quantile `q`, flush the buffer first, then:

1. Compute the target rank `t = q · n`.
2. Walk the centroids, assuming each centroid's weight sits half below its mean and half above.
   So the rank at centroid `i`'s mean is the weight of all earlier centroids plus half its own.
3. Find the two consecutive centroid means whose ranks bracket `t` and interpolate linearly
   between them.
4. Handle the special cases: a centroid of weight 1 is an exact value, so the rank steps there
   rather than ramping; below the first centroid interpolate from the minimum, and above the last
   towards the maximum.

The reverse query, the rank or CDF of a value `x`, walks the same structure and interpolates the
other way.

### Merging digests

Concatenate the centroids of the inputs and run the merge pass over them with the smallest `δ`
of the inputs. The union of valid t-digests already satisfies the size bound, so the merge pass
only re-compresses. The result is a valid t-digest but generally not the same as a digest built
from all the values at once, and it depends on the order of merges.

### Accuracy

There is no worst-case error bound; accuracy is measured empirically. In the paper's experiments
with `δ = 100` on a million values, `k2` and `k3` gave errors of a few parts per million at
`q ≤ 0.001` and `q ≥ 0.999`, while keeping only about 50 to 60 centroids. In the middle of the
distribution `k1` was somewhat more accurate. Heavily merged digests are less accurate than
digests built in one pass. If a guaranteed bound matters, use a [KLL sketch](kll-sketch.md).

### Variants

- **Clustering variant:** insert each value into the nearest centroid that still satisfies the
  size bound, often kept in a balanced tree. Sorted input makes every value a new extreme and a new
  centroid, so the digest must be compressed with a merge pass when it grows too large.
- **Stratified merge:** use a larger `δ` while values arrive and a final merge pass with the target
  `δ` when the digest is stored.

## Invariants

- The total weight of the centroids plus the buffer equals the number of values inserted.
- Centroids are sorted by mean, and `min ≤ mean ≤ max` for every centroid.
- After a merge pass, every centroid with weight above 1 satisfies the size bound, and there are
  at most `ceil(δ)` centroids.
- The estimated quantile is non-decreasing in `q`, `quantile(0) = min` and `quantile(1) = max`.
- Merging a digest with an empty one leaves its quantiles unchanged.

## Implementation pitfalls

- **NaN and float ordering.** `f64` has no total order, and `partial_cmp(...).unwrap()` is denied by
  clippy here and panics on NaN anyway. Reject NaN on insert and sort with `f64::total_cmp`.
- **Scale function domains.** Rounding can push `2q - 1` slightly outside `[-1, 1]`, making `asin`
  return NaN, and `log(q / (1 - q))` is infinite at the ends. Clamp `q` and handle the first and
  last cluster explicitly.
- **Forgetting the buffer.** Queries, serialisation and merges must flush the buffer first, or
  recent values are silently missing.
- **Dropping the minimum and maximum.** Without them, interpolation past the outer centroids
  invents values, and p0 and p100 are wrong. Keep them exactly.
- **Mean updates.** Updating a mean as `sum / weight` with a running sum loses precision on large
  values. Use the incremental form `mean += (x - mean) · w_x / (w + w_x)`.
- **Merge order and drift.** Always scanning in ascending order biases the low tail. Alternate the
  direction, and test merges in several orders.
- **Compression naming.** Implementations differ in what their "compression" parameter means and
  in which scale function they use, so serialised digests are not interchangeable. Store `δ` and
  the scale function in the format.
- **Indexing.** Walk centroids with iterators or `get` instead of `v[i]`, which clippy here denies.

## Testing

- Exact comparison: insert values from uniform, normal, exponential and heavy-tailed (Pareto)
  distributions, sort a copy, and compare estimated quantiles with the exact ones at
  `q ∈ {0.001, 0.01, 0.1, 0.5, 0.9, 0.99, 0.999}`. Measure the error in rank (`|q̂ - q|`), and set
  tolerances from repeated runs with fixed seeds.
- Tail behaviour: assert that rank errors at `0.001` and `0.999` are smaller than at `0.5` for
  `k2` and `k3`.
- Input order: repeat with sorted, reverse-sorted and random insertion orders.
- Merge: split a stream into 10 or 100 parts, digest each, merge in random orders, and check the
  merged digest's errors against the exact quantiles stay within a somewhat looser tolerance.
- Size: after a merge pass, the centroid count is at most `ceil(δ)`.
- Edge cases: an empty digest (queries return nothing), one value, all values equal, two distinct
  values, huge weights, and NaN rejected.

## Further reading

- [Computing Extremely Accurate Quantiles Using t-Digests](https://arxiv.org/abs/1902.04023),
  Dunning and Ertl, 2019.

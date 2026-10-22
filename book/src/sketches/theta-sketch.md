# Theta sketch

Estimates distinct counts by keeping only the `k` smallest hash values seen (the "k minimum
values"). Unlike HyperLogLog, two sketches support intersection and difference as well as union, so
a query can ask "how many users did both A and B".

- **Used by:** Apache DataSketches, Apache Druid's `thetaSketch` aggregator.
- **Cost:** relative error about `1/√k`; intersections of small sets have larger error.
- **Notes:** Bar-Yossef et al., 2002 (KMV); Dasgupta et al., 2016 (theta sketch framework).

## How it works

### The intuition

Hash every distinct value to a uniformly random number in `[0, 1)`. If `n` distinct values have
been seen, their hashes are spread evenly, so the `k`-th smallest lands near `k / n`. Inverting
that gives an estimate of `n`. Duplicates hash to the same number and change nothing.

Viewed differently, the hashes below a threshold `θ` are a uniform random sample of the distinct
values, taken at rate `θ`. Counting the sample and dividing by `θ` estimates the whole. Because the
sample keeps actual hash values, two sketches can be compared hash by hash, which is what makes
intersections possible.

### Layout

A sketch is a pair `(θ, S)`: a threshold `θ` with `0 < θ ≤ 1`, and the set `S` of distinct hash
values that are strictly less than `θ`. A configured **nominal size** `k` bounds `|S|`.

<figure class="diagram">
{{#include figures/theta-sketch-layout.svg}}
<figcaption>A sketch with <code>k = 8</code> after 14 distinct values. The eight smallest hashes form <code>S</code>; <code>θ</code> is the ninth smallest, which is not itself kept. The estimate, 13.8, is close to the true 14.</figcaption>
</figure>

An empty sketch has `θ = 1`. While fewer than `k + 1` distinct values have been seen, `θ` stays 1,
`S` holds every hash, and the count is exact.

### Insert

1. Hash the value with a fixed 64-bit hash and seed.
2. If the hash is `≥ θ`, discard it.
3. If it is already in `S`, discard it.
4. Add it to `S`. If `|S|` is now `k + 1`, set `θ` to the largest value in `S` (the `(k+1)`-th
   smallest hash seen) and remove that value, so `S` again holds `k` values, all below `θ`.

Keeping `S` exactly at `k` costs a search for the maximum on every replacement. Implementations
usually let `S` grow in a hash table to some multiple of `k`, then select the `(k+1)`-th smallest
with quickselect, set `θ` to it, and drop everything at or above it.

### Estimate

```text
estimate = |S| / θ
```

When `θ = 1` this is the exact count. Otherwise DataSketches bounds the relative standard error by
`1 / √(k - 1)`; for `k = 4096` that is about 1.6%. The estimate falls within one standard error of
the true value about 68% of the time, and within two about 95% of the time.

### Set operations

All inputs must use the same hash function and seed. Each operation first lowers every input to a
common threshold, so that all the samples are taken at the same rate:

- **Union:** `θ_U = min(θ_A, θ_B)`. Keep every hash from `S_A ∪ S_B` below `θ_U`. If more than
  `k` remain, lower `θ_U` to the `(k+1)`-th smallest and drop the rest. Estimate `|S_U| / θ_U`.
- **Intersection:** `θ_I = min(θ_A, θ_B)`. Keep the hashes below `θ_I` that appear in both `S_A`
  and `S_B`. Estimate `|S_I| / θ_I`.
- **Difference** (`A` not `B`): with the same `θ`, keep the hashes below it that are in `S_A` but
  not in `S_B`. This operation is not symmetric.

These are correct because any hash of `A ∩ B` below `min(θ_A, θ_B)` is below both thresholds, so it
was retained by both sketches.

<figure class="diagram">
{{#include figures/theta-sketch-set-ops.svg}}
<figcaption>Intersection and union of two sketches with <code>k = 4</code>. Both operations first lower everything to the common <code>θ = 0.47</code>, so A's hash 0.50 drops out. The intersection keeps the two shared hashes. The union has five hashes below 0.47, one more than <code>k</code>, so its <code>θ</code> falls to the fifth smallest, 0.41.</figcaption>
</figure>

The error of an intersection is relative to the size of the union, not of the intersection. The
intersection's sample is the fraction of about `k` retained hashes that happen to be shared, so if
`A ∩ B` is 1% of `A ∪ B`, only about `0.01 · k` hashes support the estimate, and its relative error
is roughly ten times larger than the union's. HyperLogLog can only estimate intersections through
`|A| + |B| - |A ∪ B|`, whose error also scales with the sizes of the inputs, so small
intersections come out no better.

### Variants

- **KMV:** the original form. Keep the `k` smallest hashes and estimate `(k - 1) / h_k`, where
  `h_k` is the `k`-th smallest. This is the same estimator as above with `θ = h_k` excluded from
  `S`.
- **Alpha sketch:** a different rule for lowering `θ`, with an estimator whose standard error is
  bounded by about `0.708 / √k` on a single, unmerged stream.
- **Compact sketch:** the immutable, serialised form: `θ`, the seed's checksum and an array of
  `S`, usually sorted. Set operations read compact sketches, and sorted arrays turn an
  intersection into a linear merge.
- **Tuple sketch:** stores a value next to each retained hash (for example, a sum), so the sample
  can answer aggregates over the distinct keys.

## Invariants

- Every value in `S` is strictly less than `θ`, and `S` has no duplicates.
- `|S| ≤ k` whenever the sketch is read (an implementation that grows lazily may hold more between
  rebuilds).
- `θ` never increases, through inserts or set operations.
- If `θ = 1`, `|S|` is the exact number of distinct values inserted.
- Union is commutative and associative, and the union of the sketches of a partitioned stream
  retains exactly the same hashes as the sketch of the whole stream.
- The hash seed is part of the serialised form, and set operations reject mismatched seeds.

## Implementation pitfalls

- **Off-by-one in `θ`.** `θ` itself must not be counted in `S`. Using the `k`-th smallest hash as
  `θ` while keeping it in `S` gives `k / h_k`, which overestimates; the unbiased forms are
  `(k - 1) / h_k`, or `k / θ` with `θ` the `(k+1)`-th smallest.
- **Floating-point thresholds.** Converting 64-bit hashes to `f64` in `[0, 1)` loses the low bits,
  because an `f64` has a 53-bit mantissa: distinct hashes collide and comparisons tie. Keep hashes
  and `θ` as integers, with the largest value standing for 1, and convert only when computing the
  estimate.
- **Seed mismatches.** Sketches hashed with different seeds give meaningless set operations, not
  an error. DataSketches stores a 16-bit hash of the seed (whose default is 9001) in every sketch
  and checks it. Rust's `DefaultHasher` and `RandomState` cannot be used, since their seeds differ
  per process.
- **Comparing before aligning `θ`.** An intersection that matches hashes before discarding those
  above the common `θ` counts some shared values from one side only and is biased.
- **Rebuild cost.** Sorting `S` on every insert past `k` makes updates `O(k log k)`. Grow the table
  and rebuild with quickselect so the cost is amortised.
- **Reporting a single number for tiny intersections.** An intersection estimate of 0 or 1 sampled
  hash is not reliable. Return confidence bounds alongside the estimate.
- **Indexing and arithmetic.** Use `get` on the retained array and checked arithmetic when
  computing table sizes from `k`, since clippy here denies slice indexing and overflowing
  arithmetic. Validate `k` when deserialising, before allocating.

## Testing

- Exact mode: insert fewer than `k` distinct values (with duplicates) and assert the estimate
  equals the `HashSet` count exactly.
- Accuracy: for cardinalities well above `k`, run 100 or more seeds and check that the relative
  errors have a standard deviation close to `1 / √(k - 1)`, and a mean near zero.
- Union equivalence: split a stream into parts, sketch each, union them, and assert the retained
  hashes and `θ` equal those of a sketch of the whole stream.
- Set operations: build two sets with a known overlap (for example 50%, 10% and 1% of the union),
  and compare union, intersection and difference estimates with exact `HashSet` answers. Check
  that the intersection error grows as the overlap shrinks, as predicted.
- Algebra: `A ∪ B` equals `B ∪ A`; `A ∩ A` estimates `|A|`; `A \ A` estimates 0.
- Edge cases: empty sketches in every operation, `k` at its smallest allowed value, and a seed
  mismatch, which must be rejected.

## Further reading

- [Theta sketches](https://datasketches.apache.org/docs/Theta/ThetaSketches.html), Apache
  DataSketches documentation.
- [A Framework for Estimating Stream Expression Cardinalities](https://arxiv.org/abs/1510.01455),
  Dasgupta, Lang, Rhodes and Thaler, 2016.

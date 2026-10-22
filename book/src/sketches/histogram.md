# Histogram

Bucketed counts of a column's values that the query optimiser uses to estimate how many rows a
predicate selects. Equi-depth histograms put the same number of rows in each bucket, which copes
with skew better than equal-width buckets.

- **Used by:** PostgreSQL `pg_stats` (most common values plus histogram bounds), MySQL 8 histograms.

## How it works

### What the optimiser needs

To choose between an index scan and a full scan, or between join orders, the planner needs the
**selectivity** of each predicate: the fraction of rows that satisfy `x = 42` or `x < 1000`. A
histogram summarises a column's value distribution in a few hundred numbers so that this fraction
can be estimated without reading the table.

### Kinds of histogram

<figure class="diagram">
{{#include figures/histogram-kinds.svg}}
<figcaption>The same 20 skewed values, <code>1 1 1 1 1 1 2 3 5 8 9 9 9 9 9 9 9 9 10 40</code>, in four buckets each way. Equal widths put 19 rows in one bucket and leave two empty. Equal depths split the sorted rows into groups of five, and only the bounds between groups are stored.</figcaption>
</figure>

- **Equi-width:** fixed ranges of the same width, with a count per range. Cheap to build, but with
  skewed data most rows land in a few buckets and the rest are wasted.
- **Equi-depth** (or equi-height): each bucket holds the same number of rows, and the histogram
  stores only the bucket boundaries. Dense regions get narrow buckets automatically.
- **Most common values plus histogram:** store the most frequent values with their exact
  frequencies, and build an equi-depth histogram over the remaining rows only. This is what
  PostgreSQL does. A very frequent value would otherwise fill several equi-depth buckets with
  identical bounds.
- **Singleton:** when a column has no more distinct values than buckets, store each value with its
  frequency. MySQL 8 chooses this type in that case, and an equi-height histogram otherwise; each
  equi-height bucket stores its lower and upper value, the cumulative frequency, and the number of
  distinct values in it.

The rest of this page follows PostgreSQL's design, because its documentation shows the
estimation arithmetic in full.

### Layout

For one column, PostgreSQL's `pg_stats` view exposes:

```text
null_frac           fraction of rows that are NULL
n_distinct          distinct non-null values (negative: a fraction of the row count)
most_common_vals    up to `statistics target` values           {EJAAAA, BBAAAA, CRAAAA, ...}
most_common_freqs   their frequencies                           {0.0033, 0.003, 0.003, ...}
histogram_bounds    up to target + 1 bounds over the other rows {0, 993, 1997, 3050, ...}
```

The **statistics target** (100 by default) bounds both the number of common values and the number
of buckets.

### Build

1. Draw a [reservoir sample](reservoir-sample.md) of `300 × target` rows. The source cites
   Chaudhuri, Motwani and Narasayya (1998): a sample of `r = 4 k ln(2n / γ) / f²` rows bounds the
   relative error in bucket size by `f` with probability `1 - γ`. With `f = 0.5`, `γ = 0.01` and a
   million rows that is about `306 k`, and the dependence on table size `n` is weak.
2. Count the NULLs to get `null_frac`, then drop them.
3. Sort the sample and count each run of equal values.
4. Choose the common values: the values frequent enough to be worth listing, up to the target.
   Their frequency is their count divided by the number of sampled rows.
5. Remove the common values from the sorted sample. Take `num_hist`, the smaller of the remaining
   distinct values and `target + 1`, as the number of bounds, and keep the first value, the last,
   and evenly spaced values between them: bound `i` is value `i · (nvals - 1) / (num_hist - 1)`.
6. Estimate `n_distinct` from the sample with the Haas and Stokes estimator,
   `n · d / (n - f1 + f1 · n / N)`, where `n` is the sample size, `N` the table's row count, `d`
   the distinct values in the sample and `f1` the values seen exactly once. Clamp it to `[d, N]`.

### Estimate equality

1. If the value is a common value, its selectivity is its listed frequency.
2. Otherwise assume the remaining rows are spread evenly over the remaining distinct values:

   ```text
   sel = (1 - null_frac - sum(mcv_freqs)) / (n_distinct - num_mcv)
   ```

### Estimate a range

For `x < v`:

1. Add up the frequencies of the common values below `v`.
2. In the histogram, find the bucket containing `v` by binary search over the bounds. Count the
   whole buckets below it, add the fraction of its bucket below `v` assuming values are spread
   linearly inside it, and divide by the number of buckets.
3. Combine: `sel = mcv_part + histogram_part · (1 - null_frac - sum(mcv_freqs))`.

The PostgreSQL documentation's example, with ten buckets and bounds
`{0, 993, 1997, 3050, ...}`, estimates `unique1 < 1000` as
`(1 + (1000 - 993) / (1997 - 993)) / 10 = 0.100697`, about 1,007 of 10,000 rows.

### Error

An equi-depth histogram with `B` buckets locates each end of a range to within one bucket, so the
error from bucketing is at most about `1 / B` of the rows per end, plus the sampling error from
step 1. Equality estimates for values outside the common list are only as good as the uniformity
assumption. Selectivities of predicates on different columns are usually multiplied, which assumes
independence; PostgreSQL's `CREATE STATISTICS` adds multi-column statistics for correlated columns.

### Variants

- **V-optimal and max-diff** histograms choose boundaries to minimise the variance of frequencies
  within buckets, or to cut where adjacent frequencies differ most. Ioannidis' survey covers them.
- **Building from a quantile sketch:** equi-depth bounds are quantiles, so a mergeable
  [KLL sketch](kll-sketch.md) per partition can produce bounds for the whole table without
  re-sampling it.

## Invariants

- Histogram bounds are sorted, and the first and last are the smallest and largest non-common
  values in the sample.
- `null_frac + sum(mcv_freqs) ≤ 1`, and every selectivity is in `[0, 1]`.
- No common value is counted in the histogram.
- `sel(x < v)` is non-decreasing in `v`, and `sel(x < v) + sel(x ≥ v) + null_frac` is 1, up to
  rounding.
- The number of bounds is at most `target + 1`.

## Implementation pitfalls

- **Zero-width buckets.** Equal adjacent bounds make the interpolation `(v - lo) / (hi - lo)` divide
  by zero. Separate frequent values into the common list, and still guard against `hi == lo`.
- **Forgetting NULLs or the common values.** The histogram part describes only the rows that are
  neither NULL nor common; leaving out the scaling factor in the combine step overestimates ranges.
- **Values outside the bounds.** Clamp to selectivity 0 or 1. Ever-increasing columns such as
  timestamps are the classic case: rows added since the last `ANALYZE` lie past the last bound and
  are estimated as absent.
- **Bound conventions.** Decide which of `<`, `≤`, `>` and `≥` each bucket boundary belongs to, and
  test predicates at exactly a bound.
- **Interpolating non-numeric types.** Linear interpolation needs a numeric position between two
  bounds; strings need an approximate mapping, or the bucket fraction must fall back to one half.
- **Integer overflow.** `v - lo` on wide integers and the bound index `i · (nvals - 1)` can
  overflow; PostgreSQL's source steps through the indexes incrementally for that reason. Use
  `checked_*` arithmetic or compute in `f64`, as clippy here denies overflowing arithmetic, and use
  `get` instead of `v[i]` when reading bounds.
- **Estimating `n_distinct` from a sample.** Distinct counts do not scale linearly from a sample.
  Clamp the estimate to `[d, N]`, and allow the user to override it.

## Testing

- Reproduce the documentation's arithmetic: with the bounds above, `x < 1000` must give 0.100697.
- Exact comparison: for uniform, Zipf-skewed and clustered columns, compare estimated and exact
  selectivities (counted over the full column) for equality and range predicates. On equi-depth
  histograms, range errors should stay within about `2 / B` of the rows plus sampling error.
- Skew: with one value making up 30% of the rows, check that it enters the common list and that its
  equality estimate is close to 0.3, and that equi-depth beats equi-width on range errors.
- Sampling: fix the sample's seed, and check that estimates vary only slightly across seeds.
- Edge cases: an empty table, an all-NULL column, a single distinct value, fewer distinct values
  than buckets, predicates on a bound, and values below the first bound or above the last.

## Further reading

- [The History of Histograms (abridged)](https://www.vldb.org/conf/2003/papers/S02P01.pdf),
  Ioannidis, 2003.
- [PostgreSQL documentation: Row Estimation
  Examples](https://www.postgresql.org/docs/current/row-estimation-examples.html). How the planner
  turns `pg_stats` into row counts.

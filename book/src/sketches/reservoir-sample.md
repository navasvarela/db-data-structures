# Reservoir sample

A uniform random sample of fixed size `k` drawn from a stream of unknown length in one pass: the
`i`-th item replaces a random slot with probability `k/i`.

- **Used by:** PostgreSQL `ANALYZE` samples table rows this way to build its statistics and
  [histograms](../sketches/histogram.md); approximate query processing.
- **Cost:** `O(k)` memory, one pass. Skip-based variants avoid drawing a random number per item.
- **Notes:** Vitter, 1985 (algorithms R and Z).

## How it works

### Layout

The reservoir is an array of up to `k` items and a counter `i` of items seen so far. The goal is
that at every moment, each of the `i` items seen has the same probability, `k / i`, of being in
the reservoir, and every subset of size `k` is equally likely.

<figure class="diagram">
{{#include figures/reservoir-sample-steps.svg}}
<figcaption>Algorithm R, below, with <code>k = 3</code>. Item <code>i</code> draws <code>j</code> from <code>1..=i</code> and enters slot <code>j</code> if <code>j ≤ 3</code>. So <code>d</code> is kept with probability 3/4 and here replaces <code>b</code>; <code>e</code> is discarded, which happens with probability 2/5; <code>f</code> is kept with probability 3/6 and replaces <code>c</code>.</figcaption>
</figure>

### Algorithm R

1. Copy the first `k` items into the reservoir.
2. For item number `i` (counting from 1, with `i > k`), draw `j` uniformly from `1..=i`.
3. If `j ≤ k`, replace slot `j` with the item; otherwise discard it.

Why it is uniform, by induction: suppose after `i - 1` items each is in the reservoir with
probability `k / (i - 1)`. Item `i` enters with probability `k / i`. An earlier item stays if it
was there and was not the slot replaced, with probability
`k/(i-1) · (1 - (k/i) · (1/k)) = k/(i-1) · (i-1)/i = k/i`.

### Skipping: Algorithms L and Z

Algorithm R draws a random number for every item, although late in a long stream almost every item
is discarded. The skip-based algorithms draw instead the number of items to skip before the next
replacement. Li's Algorithm L (1994) is the simplest:

```text
W = exp(ln(random()) / k)
loop:
    skip = floor(ln(random()) / ln(1 - W))
    advance past `skip` items; if the stream ends, stop
    replace a uniformly random slot with the next item
    W = W · exp(ln(random()) / k)
```

Here `random()` is uniform in `(0, 1)`. It needs three random numbers per replacement, and its
expected running time is `O(k (1 + log(n / k)))`. Vitter's Algorithm Z reaches the same bound with
rejection sampling. PostgreSQL's `ANALYZE` uses Algorithm Z for rows, after first choosing which
table blocks to read with Knuth's Algorithm S; it samples `300 × statistics target` rows, 30,000
with the default target of 100.

### Weighted sampling

To sample items with probability proportional to a weight `w`, Efraimidis and Spirakis' A-Res
gives each item the key `random()^(1/w)` and keeps the `k` items with the largest keys, which a
[bounded top-k heap](../query-execution/bounded-top-k-heap.md) does directly.

### Merge

Two reservoirs can only be combined correctly if the counts are known. The simplest mergeable form
gives every item a uniform random key and keeps the `k` items with the smallest keys; the merge of
two such samples is the `k` smallest keys of their union, the same rule as the
[theta sketch](theta-sketch.md). With plain Algorithm R reservoirs of `n1` and `n2` items, draw how
many of the `k` output items come from the first one from a hypergeometric distribution, then
sample that many from the first reservoir and the rest from the second.

## Invariants

- The reservoir holds `min(i, k)` items after `i` items.
- Every item in the reservoir came from the stream, and no stream position appears twice.
- Until `k` items have arrived, the reservoir is the whole stream.

## Implementation pitfalls

- **Off-by-one in the draw.** Drawing `j` from `1..i` instead of `1..=i` (or `0..i` with
  1-based slots) makes the newest item slightly too likely, and the bias is easy to miss without a
  statistical test.
- **Modulo bias.** `rng.next_u64() % i` favours small values when `i` does not divide `2^64`. Use
  the generator's uniform range sampling, or Lemire's multiply-shift with rejection.
- **`ln(0)`.** If `random()` can return 0, Algorithm L computes `ln(0) = -∞` and an infinite
  skip. Draw from the open interval `(0, 1)`.
- **Skip overflow.** A skip is a float that can be huge; convert it with a checked cast and add it
  to the position with `checked_add`, since clippy here denies overflowing arithmetic.
- **Unseeded randomness.** Pass the random generator in, so tests can fix the seed and production
  can use a fresh one.
- **Sampling the wrong unit.** Sampling blocks and then rows inside them is cheaper but not
  identical to sampling rows; PostgreSQL's source notes that its two-stage method gives every row
  the same chance without making every sample equally likely.

## Testing

- Inclusion frequency: run 100,000 trials with `n = 100` and `k = 10`, count how often each item
  ends in the reservoir, and apply a chi-square test against the expected 10% per item, with a
  fixed seed and a significance level such as 0.001.
- Pair frequency: also count how often each pair of items is sampled together; it should be
  `k (k - 1) / (n (n - 1))`. This catches samplers that are uniform per item but correlated.
- Same distribution: run Algorithm R and Algorithm L with many seeds and compare their inclusion
  frequencies.
- Merge: merge reservoirs of streams of very different lengths and check inclusion frequencies
  over the combined stream are uniform.
- Edge cases: `k = 0`, `n < k`, `n = k`, and an empty stream.

## Further reading

- [Random Sampling with a Reservoir](https://www.cs.umd.edu/~samir/498/vitter.pdf), Vitter, 1985.

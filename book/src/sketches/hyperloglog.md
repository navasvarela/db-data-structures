# HyperLogLog

Estimates the number of distinct values in a stream by tracking the longest run of leading zeros in
hashed values across many registers. Two sketches merge by taking the maximum per register.

- **Used by:** Redis `PFCOUNT`, Trino's `approx_distinct`, query optimisers estimating cardinality.
- **Cost:** standard error of about `1.04/√m` for `m` registers; Redis gets 0.81% in 12 KB.
- **Notes:** Flajolet et al., 2007.

## How it works

### The intuition

Hash every value to a uniformly random bit string. A hash that starts with `0001` (three leading
zeros, then a one) appears about once in 16 distinct values, so having seen one suggests roughly
16 distinct values. Duplicates hash identically, so they cannot change the answer. One such
observation is very noisy; HyperLogLog splits the stream into `m` substreams and averages.

### Layout

A sketch has a **precision** `p` and `m = 2^p` small registers, all zero at first. With a 64-bit
hash, each register needs 6 bits.

<figure class="diagram">
{{#include figures/hyperloglog-insert.svg}}
<figcaption>Inserting one value with <code>p = 14</code>. The first 14 bits of its hash select register <code>j = 6865</code>; the remaining 50 bits start with seven zeros, so the rank is <code>ρ = 8</code>. The register keeps the larger of its old value and <code>ρ</code>.</figcaption>
</figure>

For `p = 14`, `m = 16,384` registers of 6 bits take 12 KB, and the standard error is
`1.04 / √16384 ≈ 0.81%`, which is the Redis configuration.

### Insert

1. Hash the value to 64 bits with a fixed hash function and seed.
2. The first `p` bits give the register index `j`.
3. In the remaining `64 - p` bits, find the position of the first 1 bit, counting from 1. This is
   the rank `ρ`. If all those bits are zero, `ρ = 64 - p + 1`.
4. Set `M[j] = max(M[j], ρ)`.

### Estimate

1. Compute the raw estimate, a normalised harmonic mean of `2^M[j]`:

   ```text
   E = α_m · m² / Σ_j 2^(-M[j])

   α_16 = 0.673,  α_32 = 0.697,  α_64 = 0.709,  α_m = 0.7213 / (1 + 1.079 / m) for m ≥ 128
   ```

2. **Small range correction.** If `E ≤ 5m / 2` and `V`, the number of registers still zero, is
   not zero, return the linear counting estimate `m · ln(m / V)` instead.
3. **Large range correction.** The original paper uses a 32-bit hash, and when `E > 2^32 / 30`
   hash collisions start to hide distinct values, so it returns `-2^32 · ln(1 - E / 2^32)`. With a
   64-bit hash, collisions only matter near `2^64`, and this correction is dropped.
4. Otherwise return `E`.

The harmonic mean matters: one register with an unlucky large `ρ` would dominate an arithmetic
mean of `2^M[j]`, but contributes almost nothing to `Σ 2^(-M[j])`.

### Merge

Two sketches with the same `p`, hash function and seed merge by taking `max(A[j], B[j])` for every
register. The result is exactly the sketch of the union of the two streams, whatever the order of
inserts and merges. This is what makes HyperLogLog useful across segments and machines: store one
sketch per partition and merge at query time.

<figure class="diagram">
{{#include figures/hyperloglog-merge.svg}}
<figcaption>Merging two sketches with <code>m = 8</code>. Each register of the result is the larger of the two inputs; the colour shows which input it came from, and ties are left plain.</figcaption>
</figure>

A sketch can be reduced from precision `p'` to a smaller `p`: each new register is fed by `2^(p'-p)`
old ones, and the bits that moved from the index into the remainder must be folded into `ρ`.
Merging sketches of different precisions means reducing the larger one first.

### Error

The relative standard error is about `1.04 / √m`. Halving the error costs four times the
registers. The error does not depend on the number of distinct values, only on `m`, except in the
small range where the corrections above apply.

### Variants

- **HyperLogLog++** (Heule, Nunkesser and Hall, 2013): a 64-bit hash with no large range
  correction; an empirical bias correction, interpolated from tables measured per precision, for
  raw estimates up to `5m`; and an empirically chosen threshold for switching from linear counting
  (11,500 for `p = 14`).
- **Sparse representation:** while few registers are set, store `(index, ρ)` pairs in a sorted,
  compressed list instead of `m` registers, at a higher precision `p'` (the paper uses
  `p' = 25`), and convert to the dense array when the list would outgrow it.
- **Improved estimators:** Ertl, 2017, derives a bias correction from theory rather than tables,
  and a maximum likelihood estimator.
- **Packed registers:** 6 bits per register, or 4 bits relative to a shared offset, instead of a
  byte each.

## Invariants

- Registers only grow: an insert or merge never decreases `M[j]`.
- Every register is in `0..=64 - p + 1`.
- Inserting a value already seen leaves the sketch unchanged (idempotence).
- `merge(A, B) == merge(B, A)`, `merge(A, A) == A`, and merging is associative.
- The sketch of a stream equals the merge of the sketches of any partition of that stream.
- `p`, the hash function and the seed are part of the serialised form, and merge refuses
  sketches whose values differ.

## Implementation pitfalls

- **Unstable or weak hashing.** Rust's `DefaultHasher` and `RandomState` are seeded per process,
  so sketches from two processes cannot be merged and a stored sketch cannot be extended. Use a
  specified 64-bit hash with a fixed seed. A weak hash that leaves the low bits non-random biases
  `ρ`.
- **Skipping the small range correction.** With few values most registers are zero and the raw
  estimate is far too high: with no input at all it is about `0.7m`, not 0.
- **Mixing up index and rank bits.** The index and `ρ` must come from disjoint bits. Taking `ρ`
  from the whole hash, including the index bits, biases every estimate.
- **All-zero remainder.** `leading_zeros` on a zero remainder returns its full width; check that
  `ρ` then comes out as `64 - p + 1`, and that the shift that removes the index bits does not
  shift in ones.
- **Shifts and widths.** `1 << 64` and `x >> 64` are invalid in Rust. Compute `2^(-M[j])` as a
  floating-point power, or with `f64::powi`, and guard shifts by `p` and `64 - p`. Clippy here
  denies arithmetic that can overflow, so prefer `checked_shl` and `wrapping_*` where intended.
- **Precision out of range.** Validate `p` (the paper allows 4 to 16; HyperLogLog++ goes higher)
  when constructing and when deserialising, before allocating `2^p` registers.
- **Register access.** Use `get` and `get_mut` on the register array; clippy here denies
  `v[i]`. The index is always below `m` by construction, but the type system cannot see that.

## Testing

- Exact comparison: for cardinalities from 0 to several million, insert distinct values and
  compare the estimate with the exact count from a `HashSet`. Over many seeds, the relative errors
  should have a standard deviation close to `1.04 / √m`, for example within 20%.
- Bias: average the estimates over 100 or more seeds at each cardinality; the mean relative error
  should stay near zero, especially around the switch-over from linear counting.
- Duplicates: inserting each value ten times must give the same sketch as inserting it once.
- Merge equivalence: split a stream into parts, sketch each, merge, and assert the registers equal
  those of a sketch of the whole stream. Check commutativity and idempotence too.
- Small cardinalities: 0 must estimate 0, and with `p = 14` estimates up to a few hundred should be
  within a few percent, since linear counting is very accurate when most registers are empty.
- Serialisation: round-trip a sketch through bytes, merge it with a fresh one, and check that a
  sketch with a different `p` or seed is rejected.

## Further reading

- [HyperLogLog: the analysis of a near-optimal cardinality estimation
  algorithm](https://algo.inria.fr/flajolet/Publications/FlFuGaMe07.pdf), Flajolet, Fusy, Gandouet
  and Meunier, 2007.
- [HyperLogLog in
  Practice](https://static.googleusercontent.com/media/research.google.com/en//pubs/archive/40671.pdf),
  Heule, Nunkesser and Hall, 2013. The engineering fixes (64-bit hashes, bias correction, a sparse
  representation) that production versions use.

# Xor and Ribbon filters

Static filters built once from a known key set by solving a system of equations. They use less space
than Bloom filters, which suits immutable SSTables.

- **Used by:** RocksDB offers Ribbon filters as a Bloom alternative.
- **Notes:** Graf and Lemire, 2020 (Xor); Dillinger and Walzer, 2021 (Ribbon).

## How it works

Both filters share one idea. Each key `x` gets an `r`-bit **fingerprint** `fp(x)` and a small set
of positions in an array `Z`. Construction chooses the contents of `Z` so that, for every key in
the set, xoring the entries at its positions gives back its fingerprint:

```text
Z[p1(x)] xor Z[p2(x)] xor ... = fp(x)        for every key x in the set
```

A query evaluates the same xor and compares it with the fingerprint. A key in the set always
matches. A key not in the set gets an essentially random `r`-bit value, which matches its
fingerprint with probability `2^-r`. Nothing is inserted after construction, which is why these
filters fit an [SSTable](../lsm-trees/sstable.md) and not a memtable.

The two filters differ in how positions are chosen and how the equations are solved. An xor filter
uses three random positions and solves by peeling. A Ribbon filter uses positions close together
and solves by Gaussian elimination.

### Xor filter layout

The array `Z` has `c = floor(1.23 · n) + 32` slots of `r` bits, split into three equal segments. A
query for `x` reads one slot per segment and checks `fp(x) == Z[h0(x)] xor Z[h1(x)] xor Z[h2(x)]`.

<figure class="diagram">
{{#include figures/xor-filter-query.svg}}
<figcaption>A query with 8-bit fingerprints, on a toy array of three segments of six slots. The three slots of <code>x</code> xor to <code>8e</code>, its fingerprint. A key not in the set would read three unrelated slots and match its own fingerprint with probability 1/256.</figcaption>
</figure>

Each of the three hashes picks a slot in its own segment, so a key's three slots are always
distinct. The array has `1.23 n` slots because peeling (below) only succeeds reliably when there
are at least about 1.22 slots per key; the extra 32 slots help small sets.

The cost is `1.23 · r` bits per key: with 8-bit fingerprints, 9.84 bits per key for a 0.39%
false-positive rate; with 16-bit fingerprints, about 19.7 bits for 0.0015%. A query is always
exactly three memory reads, with no branches.

### Xor filter construction (peeling)

1. Pick a seed and compute `h0`, `h1`, `h2` for every key.
2. For every slot, record how many keys map to it. A common trick is to store, next to the count,
   the xor of the hashes of those keys: when the count is 1, that xor is the hash of the only
   key, so no per-slot list of keys is needed.
3. Put every slot whose count is 1 into a queue.
4. Pop a slot `i`. If its count is still 1, it holds a single key `x`: push `(x, i)` onto a stack,
   then remove `x` from its three slots (decrement each count, xor out its hash). Any slot whose
   count drops to 1 joins the queue.
5. When the queue is empty, construction succeeded if the stack holds all `n` keys. Otherwise
   the remaining keys form a cycle that cannot be peeled: pick a new seed and start again.
6. Assign in reverse: pop `(x, i)` from the stack and set
   `Z[i] = fp(x) xor Z[h0(x)] xor Z[h1(x)] xor Z[h2(x)]`, treating `Z[i]` itself as 0.

<figure class="diagram">
{{#include figures/xor-filter-peeling.svg}}
<figcaption>Peeling four keys over nine slots. Slots 1, 2 and 3 start with a single key; slot 5 does too, but by the time it is popped <code>d</code> has been removed and its count is 0. Peeling <code>c</code> drops slot 4 to count 1, which lets <code>b</code> be peeled last. The highlighted edge of each key is the slot it is assigned. Slot 8 is used by no key and stays 0.</figcaption>
</figure>

Step 6 is correct because the stack reverses the peeling order. When `(x, i)` was pushed, `i` was
used by no key still left in the graph, so no key assigned after `x` touches `Z[i]`, and `x`'s
equation stays satisfied to the end.

For large sets the first seed almost always works. For small sets the paper measured a success
probability above 0.8 per attempt with `c = 1.23 n + 32`, so the expected number of attempts is
below 1.25.

### Ribbon filter layout

A Ribbon filter replaces the three random positions with a **band**. Each key gets a start
position `s(x)` and a `w`-bit coefficient row `c(x)`, whose first bit is forced to 1. The key's
positions are `s(x) + j` for every set bit `j` of `c(x)`. The ribbon width `w` is typically 64 or
128, one or two machine words.

<figure class="diagram">
{{#include figures/ribbon-filter-band.svg}}
<figcaption>Coefficient rows of five keys with <code>w = 8</code>, drawn at their start positions. Key <code>x</code> starts at row 42 with <code>c(x) = 10110010</code>, so its positions are rows 42, 44, 45 and 48. Sorted by start position, the rows form a band along the diagonal.</figcaption>
</figure>

`Z` has `m` rows of `r` bits, with `m` slightly larger than `n`. Sorted by start position, the
equations form a matrix whose non-zero entries lie in a diagonal band of width `w`, and Gaussian
elimination on such a matrix never creates entries outside the band. That keeps construction
linear and lets `m` be within a few percent of `n`, against 23% extra for an xor filter.

### Ribbon construction (incremental banding)

The solver keeps a system `M` with one row per position. Row `i` is empty or holds an equation
`(c_i, b_i)` whose coefficient starts at column `i`. To add a key:

1. Set `i = s(x)`, `c = c(x)`, `b = fp(x)`.
2. Loop:
   1. If row `i` of `M` is empty, store `(c, b)` there. The key is added.
   2. Otherwise xor the stored row into the equation: `c ^= c_i`, `b ^= b_i`. Both started with
      a 1 bit, so `c` now starts with 0.
   3. If `c` is all zeros: when `b` is also zero the equation was redundant, which is fine; when
      it is not, the system is inconsistent and construction fails.
   4. Otherwise find the first set bit of `c` at offset `j`, shift `c` by `j`, add `j` to `i`, and
      loop.

When every key is added, **back-substitute** from the last row to the first. For row `i`, if
`M` has an equation there, set `Z[i] = b_i` xor the xor of `Z[i + j]` for every set bit `j > 0` of
`c_i`. If row `i` is empty, `Z[i]` is free and can take any value. Queries then evaluate the xor
from the layout section.

On failure, start again with a new seed or a slightly larger `m`.

### Variants

- **Xor+:** compresses the mostly empty third segment of an xor filter with a rank structure, for
  about `1.0824 · r + 0.5125` bits per key, at the cost of slower queries.
- **Binary fuse filters** (Graf and Lemire, 2022): the xor filter with spatially coupled segments.
  It is within 13% of the space lower bound with three positions, or 8% with four, and builds
  faster.
- **Homogeneous Ribbon:** solve `c(x) · Z = 0` for every key, with no fingerprints at all.
  A homogeneous system always has a solution, so construction never fails. Free rows must be
  filled with random values, and a query answers "maybe present" when the xor is zero.
- **RocksDB's Ribbon filter:** available since version 6.15.0. Its documentation reports about
  30% less space than its Bloom filter at the same false-positive rate (about 7 bits per key for
  1%), for about three to four times the CPU when building filters.

## Invariants

- Every key in the build set satisfies its equation: `query(x)` returns "maybe present" for all
  of them.
- `Z`, `r`, the array size and the seed are fixed after construction and are part of the
  serialised form.
- Xor filter: a key's three slots lie in three different segments.
- Ribbon: `s(x) + w ≤ m` for every key, every coefficient row starts with a 1 bit, and every
  non-empty row `i` of `M` holds a coefficient whose first set bit is at column `i`.
- During peeling, the slot counts always equal the number of not-yet-peeled keys mapping to each
  slot.

## Implementation pitfalls

- **Duplicate keys.** Two identical keys share all three slots, so no slot ever reaches count 1,
  and xor construction fails on every seed. Deduplicate the key hashes before building. Ribbon
  treats a duplicate as a redundant equation, but deduplicating still saves work.
- **Unbounded retries.** A construction loop that retries forever turns a bug (such as duplicate
  keys) into a hang. Cap the number of seeds and return an error, since this crate denies
  `panic!`.
- **Forgetting the seed.** The seed that succeeded must be written with the filter. A reader that
  rehashes with a different seed sees random positions and reports false negatives.
- **Unstable hashing.** Rust's `DefaultHasher` and `RandomState` are seeded per process and may
  change between Rust versions. Use a specified 64-bit hash with an explicit seed.
- **Slot reduction.** Map a hash onto a segment with Lemire's multiply-shift, `(h · len) >> 32`
  on a 32-bit `h`, instead of `%`. Compute it as a widening multiplication, since clippy here
  denies arithmetic that can overflow.
- **Ribbon start positions out of range.** `s(x)` must lie in `0..=m - w`, or the band runs past
  the end of `Z`. Read rows with `get` rather than `Z[i]`, so a bad start position becomes an
  error instead of a panic.
- **Bit order in the band.** Decide once whether bit 0 of `c` is the leftmost column, and use the
  same convention for the shift in step 4, back-substitution and queries. Mixing them produces a
  filter that passes small tests and fails on large ones.
- **Free rows in homogeneous Ribbon.** Every right-hand side is zero, so if empty rows are also
  left at zero, back-substitution produces the all-zero `Z` and the filter answers "maybe present"
  for every key. Fill free rows with random bits.
- **Correlated fingerprints and positions.** If `fp(x)` reuses the hash bits that chose the
  positions, absent keys that land on the same slots as a stored key also tend to share its
  fingerprint. Derive the fingerprint from different bits, or from a second mix of the hash.

## Testing

- No false negatives: build from a random set and assert every key answers "maybe present".
- False-positive rate: query 100,000 absent keys and check the measured rate is close to `2^-r`,
  for example within 1.5 times (0.39% for 8-bit fingerprints).
- Construction success: build many small sets (10 to 10,000 keys) with fixed seeds and record how
  many attempts each needed. Most should succeed first time; none should exceed the retry cap.
- Duplicates: a key set with a repeated key must either succeed after deduplication or return an
  error, never loop.
- Ribbon solver: after back-substitution, re-evaluate every stored equation against `Z` before
  answering any query. Compare against a slow dense Gaussian elimination on small sets.
- Serialisation: write the filter with its seed, read it back in a fresh process and repeat the
  first two checks.
- Homogeneous Ribbon: check that the measured false-positive rate on absent keys is close to
  `2^-r`, not 100%, which catches free rows left at zero.
- Edge cases: zero keys, one key, and key counts around segment-size rounding boundaries.

## Further reading

- [Xor Filters: Faster and Smaller Than Bloom and Cuckoo Filters](https://arxiv.org/abs/1912.08258),
  Graf and Lemire, 2020.
- [Ribbon filter: practically smaller than Bloom and Xor](https://arxiv.org/abs/2103.02515),
  Dillinger and Walzer, 2021.

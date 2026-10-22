# Learned index

A model of the key distribution that predicts where a key sits in a sorted array, plus a bounded
search around the prediction to correct its error. Piecewise linear models are the practical choice:
each segment covers a run of keys and guarantees the prediction is within `ε` positions.

- **Used by:** mostly research systems and the PGM-index library. It suits immutable sorted data,
  such as an [SSTable](../lsm-trees/sstable.md) index or a [sorted
  column](../analytical-storage/sparse-primary-index.md).
- **Cost:** `O(log ε)` search after the prediction; the model can be orders of magnitude smaller
  than a B-tree over the same keys. Inserts need a buffer or a rebuild.
- **Notes:** Kraska et al., 2018 (recursive model index); Ferragina and Vinciguerra, 2020
  (PGM-index).

## How it works

### The idea

Take a sorted array of `n` keys. The function that maps a key to its position, its *rank*, is a
non-decreasing step function: the cumulative distribution of the keys, scaled by `n`. A B-tree is
one way to approximate it. A learned index approximates it with a model instead, then corrects
the model's error with a short search:

<figure class="diagram">
{{#include figures/learned-index-segments.svg}}
<figcaption>Thirty keys plotted by key and position, approximated by two segments starting at <code>key₁</code> and <code>key₂</code>, each predicting <code>pos ≈ slope · (k − key) + intercept</code>. With <code>ε = 2</code>, every key lies inside its segment's band of ± 2 positions. A lookup for <code>k</code> reads its prediction off segment 2 and only searches the window <code>pos ± ε</code>.</figcaption>
</figure>

This page follows the PGM-index (Ferragina and Vinciguerra, 2020), whose models come with a
guaranteed error bound, and then describes the recursive model index as a variant.

### Layout

- **Data:** the sorted array of keys (with their values, or positions of them). It is never
  modified in place.
- **Segments:** a *segment* is a triple `(key, slope, intercept)`. It covers the keys from its own
  `key` up to the next segment's, and predicts the position of a key `k` in that range as
  `slope · (k − key) + intercept`. A segment is *`ε`-approximate* if every prediction it makes for a
  key in its range is within `ε` positions of the true rank.
- **Levels:** the segments over the data form the bottom level. Their first keys form a much
  shorter sorted array, which gets its own segments, and so on, until a level has one segment, the
  root. Each level is to the level below what the bottom level is to the data.

The PGM paper shows that each segment of an optimal model covers at least `2ε` keys, so the bottom
level has at most `n / (2ε)` segments, and in practice far fewer on regular data.

### Build: fitting segments

The PGM-index uses an algorithm from computational geometry (O'Rourke, 1981) that, in one pass
and linear time, produces the minimum number of `ε`-approximate segments. It grows the convex hull
of the points `(key, rank)` and closes a segment when the hull no longer fits in a strip of height
`2ε`.

A simpler greedy method, in the spirit of the FITing-tree's *shrinking cone*, is a good first
version. It produces more segments than the optimum but keeps the same `ε` guarantee:

1. Start a segment at the first remaining point `(x₀, y₀)`. Set the allowed slope range to
   `[0, +∞)`.
2. For each next point `(x, y)`, the slopes that keep it within `ε` are those between
   `(y − ε − y₀) / (x − x₀)` and `(y + ε − y₀) / (x − x₀)`. Intersect the allowed range with them.
3. If the intersection is empty, close the segment with any slope in the previous range (the
   middle, for example), and start a new segment at this point. Otherwise, continue.

<figure class="diagram">
{{#include figures/learned-index-cone.svg}}
<figcaption>The shrinking cone with <code>ε = 1</code>. Each point after <code>(x₀, y₀)</code> allows only the slopes whose line passes through its error bar, so the cone of allowed slopes narrows with every point. The last point's bar lies entirely above the cone: the segment is closed before it, and a new one starts there.</figcaption>
</figure>

Building the upper levels repeats the fit on the first key of each segment, with the index of
that segment as its "position".

### Lookup

1. At the root, predict the position of `k` in the level below.
2. Search the window `[pos − ε, pos + ε]` of that level, clamped to its bounds, for the last
   segment whose first key is at most `k`.
3. Use that segment to predict the position in the next level down, and repeat until the data
   array.
4. In the data, search the final window for the first key not smaller than `k`. Either it equals
   `k`, or `k` is absent and this is where it would go.

<figure class="diagram">
{{#include figures/learned-index-levels.svg}}
<figcaption>A lookup in a two-level index with <code>ε = 2</code>. The root predicts a position among the segments, and a binary search of the five entries around it finds the last segment whose first key is at most <code>k</code>. That segment predicts a position in the data, and a second search of five entries finds the key.</figcaption>
</figure>

Each step is a binary search over at most `2ε + 1` entries, so a lookup costs `O(log ε)` per level.
For a key that falls between the last key covered by a segment and the first key of the next,
the paper takes the smaller of the segment's prediction and the next segment's prediction for its
own first key, so the extrapolated line cannot overshoot into the next segment's range.

### Insert and delete

The structure is static: a segment can cover any number of keys, so there is no node to split.
Two strategies from the PGM paper:

- **Appends in key order** (time series): extend the last segment if the new key fits within `ε`,
  otherwise start a new one and insert its key into the level above in the same way.
- **Inserts anywhere:** the *logarithmic method*. Keep a series of static indexes over sets of
  size 0 or `2^i`. An insert merges the sets below the first empty one, plus the new key, into a
  new static index of that size. A delete inserts a tombstone. A lookup queries every set.

This is the same shape as an [LSM tree](../lsm-trees/lsm-tree.md), and it is how a learned index
fits immutable [SSTables](../lsm-trees/sstable.md): build one per file, never update it.

### Variants

- **Recursive model index** (Kraska et al., 2018): a fixed number of stages, each an array of
  models. The model at stage 0 sees every key; the model it predicts picks the model at the next
  stage, `floor(M · f(k) / n)` for a stage of `M` models. The last stage predicts the position.
  Models are trained stage by stage, and each last-stage model stores the largest over- and
  under-prediction it made on the stored keys, which bounds the final search. A *hybrid* index
  replaces a last-stage model whose error exceeds a threshold with a B-tree. Unlike the PGM-index,
  error bounds are measured after training rather than guaranteed by construction.
- **Search strategy:** besides plain binary search over the window, the RMI paper describes a
  binary search whose first probe is the predicted position, and notes that exponential search
  outwards from the prediction needs no stored error bounds.

## Invariants

- The data array is sorted, and every level's segments are sorted by first key.
- For every stored key at position `i`, the bottom level's prediction `p` satisfies
  `|p − i| ≤ ε`, after rounding to an integer exactly as lookups do.
- At every upper level, the prediction for a segment's first key is within `ε` of that segment's
  index in the level below.
- The top level has exactly one segment.
- A lookup of any key, stored or not, returns the same position as a binary search of the whole
  array would.

## Implementation pitfalls

- **Floating-point precision.** An `f64` holds integers exactly only up to `2^53`. Converting
  large `u64` keys to `f64` merges neighbouring keys, and a slope times a large key loses digits.
  Compute `k − key` in integers first, convert the difference, and check the `ε` bound on the
  rounded result, not on the exact line.
- **Building and querying with different arithmetic.** The guarantee holds only if lookups
  compute predictions exactly as the builder checked them. Share one prediction function.
- **Window bounds.** `pos − ε` underflows near the start and `pos + ε` passes the end near the
  finish. Clamp with `saturating_sub` and `min(len - 1)`, and remember that the window is `2ε + 1`
  entries, not `2ε`.
- **Float-to-integer casts.** `as` saturates out-of-range values and turns `NaN` into 0 without an
  error, and clippy's pedantic group warns about these casts. Clamp explicitly, so a bad
  prediction cannot silently become position 0.
- **Duplicate keys.** Two points with the same key make the slope calculation divide by zero, and
  the rank of a duplicated key is ambiguous. Deduplicate (keeping the first position) before
  fitting, or treat equal keys as one point.
- **Tiny inputs.** Zero or one key leaves the slope undefined; special-case them.
- **Non-integer keys.** The model needs a number. Byte-string keys can be mapped to an integer
  from a fixed-length prefix, but keys that share that prefix collapse to the same number and the
  search window has to cover them all.
- **Non-monotonic models.** The RMI paper notes that a model must be monotonic for its stored
  error bounds to hold for keys that are not in the array. Linear segments are monotonic as long
  as slopes are not negative.
- **On disk.** If the model is persisted next to an SSTable, serialise slopes and intercepts as
  exact bit patterns (`f64::to_le_bytes`), never as decimal text.

## Testing

- Reference model: `slice::partition_point` over the same sorted array. For every stored key, and
  for many absent keys (below the minimum, above the maximum, and between neighbours), the
  learned index must return the same position.
- Error bound: for every stored key, assert the prediction at each level is within `ε`.
- Distributions: uniform, dense consecutive integers (one segment should suffice), clustered,
  exponentially spaced, keys near `u64::MAX`, and a few extreme outliers.
- Segment count: for the optimal algorithm, assert that the bottom level has at most `n / (2ε)`
  segments; for the shrinking cone, compare its count against the optimal one.
- Parameters: `ε = 1` and very large `ε`, plus arrays of zero, one and two keys.
- Serialisation: write the model, read it back, and repeat every lookup.

## Further reading

- [The Case for Learned Index Structures](https://arxiv.org/abs/1712.01208), Kraska et al., 2018.
- [The PGM-index: a fully-dynamic compressed learned index with provable worst-case
  bounds](https://www.vldb.org/pvldb/vol13/p1162-ferragina.pdf), Ferragina and Vinciguerra, 2020.
  See also the [project site](https://pgm.di.unipi.it/).

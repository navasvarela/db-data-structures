# Space-filling curves

Maps multidimensional points to one dimension (Z-order, Hilbert curve, geohash) so that points close
in space tend to be close in key order. Any ordered index can then serve spatial queries.

- **Used by:** Delta Lake Z-ordering, Redis geospatial commands (a 52-bit geohash as a sorted-set
  score), geohash grid aggregations in Elasticsearch.

## How it works

### Layout

A space-filling curve is a function from a grid of cells to a line, `key = curve(x, y)`, that
visits every cell exactly once. It is not stored anywhere: the "structure" is the key it produces,
which is then stored in an ordinary ordered index such as a
[B+ tree](../page-based-trees/b-plus-tree.md), an [SSTable](../lsm-trees/sstable.md) or a sorted
file.

Coordinates are first *quantised* to unsigned integers of `b` bits per dimension, so the grid has
`2^b` cells along each axis. Two curves on a 4 × 4 grid (`b = 2`), with `y = 0` as the top row:

<figure class="diagram">
{{#include figures/space-filling-curves-grids.svg}}
<figcaption>Each cell shows its key in the corner, and the line visits the cells in key order. Each 2 × 2 quadrant is one run of four keys on both curves.</figcaption>
</figure>

Both have the property that matters: the cells of any aligned square of side `2^l` occupy one
contiguous run of keys. They differ at the edges of those squares. Z-order jumps (from 5 to 6 above
means going from `(3, 0)` to `(2, 1)`, and from 7 to 8 crosses the grid); on the Hilbert curve,
consecutive keys are always neighbouring cells.

### Quantising coordinates

1. Choose a fixed range for each dimension, for example longitude `[-180, 180]`.
2. Map each value to an integer in `0..2^b` by scaling and rounding down, clamping the maximum to
   `2^b - 1`.
3. For floating-point columns with no natural range, an order-preserving bit mapping works too:
   take the IEEE 754 bits, flip only the sign bit for non-negative values, and flip all bits for
   negative values. Unsigned order on the result is then numeric order on the floats.

### Z-order encode

Interleave the bits: bit `i` of `x` goes to bit `2i` of the key, and bit `i` of `y` to bit
`2i + 1`. With 32-bit coordinates the key is a `u64`; with three dimensions, 21 bits each fit.
Instead of a loop over bits, the usual implementation spreads each coordinate with a few
shift-and-mask steps ("magic numbers") and ORs the results. Decoding reverses the steps.

<figure class="diagram">
{{#include figures/space-filling-curves-interleave.svg}}
<figcaption>The key of cell <code>(2, 1)</code> in the grid above. Reading the key from its high bit, the bits alternate between <code>y</code> and <code>x</code>.</figcaption>
</figure>

### Hilbert encode

The Hilbert curve is built recursively: each quadrant holds a copy of the whole curve at half the
size, rotated or reflected so that the copies join end to end. Encoding walks the bits from the
most significant:

1. Set `d = 0`, and `s` to the highest bit, `2^(b-1)`.
2. Read the quadrant from the current bit of each coordinate: `rx = (x & s) != 0`,
   `ry = (y & s) != 0`.
3. Add the quadrant's position along the curve, `s² · ((3 · rx) XOR ry)`, to `d`.
4. Rotate or reflect `x` and `y` into that quadrant's orientation: if `ry` is 0, reflect both
   coordinates when `rx` is 1, then swap `x` and `y`.
5. Halve `s` and repeat until it is zero. `d` is the key.

This is the two-dimensional version; Skilling's algorithm generalises it to `n` dimensions.

### Geohash

A geohash is a Z-order key on longitude and latitude written as text: interleave the bits starting
with longitude, then encode each group of 5 bits as one character of a base-32 alphabet
(`0123456789bcdefghjkmnpqrstuvwxyz`, which skips `a`, `i`, `l` and `o`). Every extra character
divides the cell into 32 smaller cells, and two points in the same cell share the geohash prefix.
Redis keeps the 52-bit integer instead of the text, as the score of a sorted-set member.

### Range query

A rectangle does not map to one key range. With Z-order, its keys all lie between the key of its
lower corner and the key of its upper corner, because Z-order never decreases when a coordinate
increases. But that range also contains many cells outside the rectangle. Three ways to deal with
it, from simplest to best:

1. **Scan and filter:** scan from the lower-corner key to the upper-corner key, and drop points
   outside the rectangle. Correct, but the scan can be much larger than the result.
2. **Skip with BIGMIN:** scan as above, but when a key falls outside the rectangle, compute the
   smallest key greater than it that is inside the rectangle (Tropf and Herzog's BIGMIN), and seek
   there. In the Z-order grid above, the window `x, y ∈ [1, 2]` spans keys 3 to 12, but the scan
   visits 3, jumps from 4 to 6, from 7 to 9 and from 10 to 12.
3. **Decompose into cells:** cover the rectangle with aligned squares, as a quadtree would, and
   turn each square into its contiguous key range. Limit the number of ranges, accepting some
   extra cells at the edges, since each range costs a seek. Redis does a coarse version of this:
   for a radius or box search it checks the geohash cell of the centre and its eight neighbours,
   each as one score range of the sorted set, and filters the results by exact distance.

<figure class="diagram">
{{#include figures/space-filling-curves-bigmin.svg}}
<figcaption>The BIGMIN scan from approach 2. Each time the scan reads a key outside the window (4, 7, 10), BIGMIN gives the next key inside it, and the scan seeks there.</figcaption>
</figure>

The last approach works for the Hilbert curve as well; BIGMIN is specific to Z-order.

### Clustering files

In analytical storage the curve is not an index at all. Sorting rows by the Z-order (or Hilbert)
key of several columns before writing them into files makes each file cover a compact region in all
those columns, so per-file minimum and maximum statistics, the
[zone maps](../analytical-storage/zone-maps.md), can skip files for filters on any of them. Sorting
by one column only helps filters on that column.

### Variants

- **Hilbert versus Z-order:** Hilbert keeps neighbouring cells closer together, so a range query
  touches fewer separate key runs, but its encoding costs more and it has no BIGMIN shortcut.
- **More dimensions:** both curves generalise, but with a fixed key width each extra dimension
  costs bits of precision, and locality degrades as dimensions grow.

## Invariants

- Encoding is a bijection between the quantised grid and `0..2^(b·d)` for `d` dimensions: decoding
  an encoded cell gives the same cell back.
- All cells of an aligned square of side `2^l` share the top `b·d - l·d` bits of their key, and so
  form one contiguous key range.
- Z-order is monotonic in each coordinate: if every coordinate of `p` is at most the matching
  coordinate of `q`, then `z(p) ≤ z(q)`.
- Consecutive Hilbert keys are cells that share an edge.
- A range query's set of key ranges covers every cell of the query rectangle.

## Implementation pitfalls

- **Inconsistent quantisation.** The writer and every reader must use the same ranges, rounding
  and number of bits, or keys stop matching. Store them with the data.
- **Points at the upper edge.** Scaling the maximum value of a range gives exactly `2^b`, one past
  the last cell. Clamp it.
- **Float corner cases.** `-0.0` and `+0.0` map to different bit patterns, and `NaN` has no place in
  the order. Normalise `-0.0`, and reject or separately handle `NaN`.
- **Which dimension goes first.** Swapping which coordinate supplies the lower bit of each pair
  gives a different, equally valid curve. Pick one, document it next to the encoder, and use it
  everywhere; geohash fixes it as longitude first.
- **Shifts in this crate.** Clippy's `arithmetic_side_effects` lint does not flag `<<` or `>>` on
  integers, so the shift-and-mask steps pass the crate's lints as written. A shift by an amount at
  least the bit width still panics in debug builds, which matters only for a variable amount; use
  `checked_shl` there. The fast BMI2 instructions for bit interleaving (`_pdep_u64` and
  `_pext_u64`) are exposed as `std::arch` intrinsics that generally need `unsafe` to call, so
  they are off limits here.
- **Geohash edges.** Two nearby points on opposite sides of a cell boundary, such as the
  antimeridian or the equator, can have no common prefix at all. Proximity queries must look at
  neighbouring cells, not only at the prefix of the query point.
- **Over-fine decomposition.** Covering a rectangle exactly with aligned squares can produce
  thousands of tiny ranges. Cap the count and accept false positives.

## Testing

- **Round trips.** For random cells, and for every cell of a small grid, check `decode(encode(c))`
  is `c`, for both curves, including the grid's corners and the maximum coordinate.
- **Known values.** Check the 4 × 4 grids above, and for geohash, published test vectors from a
  reference implementation.
- **Curve properties.** Exhaustively for small grids: Z-order monotonicity; consecutive Hilbert keys
  are adjacent cells; each aligned square is one contiguous key range.
- **Range queries against brute force.** For random points and random rectangles, compare the
  result of each range strategy with a linear scan, and also record how many keys each strategy
  visited.
- **BIGMIN.** For every key in a small grid and random rectangles, compare BIGMIN with a linear
  search for the next key inside the rectangle.

## Further reading

- [Z-order curve](https://en.wikipedia.org/wiki/Z-order_curve), Wikipedia. Bit interleaving and how
  to answer range queries over Z-ordered keys.
- Morton, *A Computer Oriented Geodetic Data Base and a New Technique in File Sequencing*, IBM,
  1966.

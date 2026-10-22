# Bitmap index

One bitmap per distinct value, with a bit per row. Predicates combine with bitwise `AND` and `OR`.
Compressed formats such as Roaring split the row space into chunks of 2^16 and store each chunk as a
sorted array, a bitmap or a list of runs, whichever is smallest.

- **Used by:** Apache Druid, Apache Pinot, Lucene filters, ClickHouse `groupBitmap`.
- **Notes:** Chambi, Lemire et al., 2016 (Roaring).

## How it works

### Layout of the index

For a column with distinct values `v₁ … vₖ` over a segment of `n` rows, the index keeps `k` bitmaps
of `n` bits, where bit `r` of bitmap `vᵢ` is set when row `r` holds `vᵢ`. NULL gets its own
bitmap, because it is not a value and must not be produced by `NOT`.

<figure class="diagram">
{{#include figures/bitmap-index-bitmaps.svg}}
<figcaption>A <code>color</code> column with one bitmap per value and one for NULL; row 4 is NULL. The predicate <code>color IN ('R', 'B')</code> is the OR of the R and B bitmaps, and its five set bits are the matching rows.</figcaption>
</figure>

A dictionary maps each value to its bitmap. A predicate becomes bitwise operations: `color IN
('R', 'B') AND size = 'L'` is `(R OR B) AND L`; `COUNT(*)` of the result is its cardinality, and
the set bits are the row numbers to fetch. Uncompressed bitmaps cost `k · n` bits, which is
acceptable only for a handful of values; compression is what makes the index practical.

### Layout of a Roaring bitmap

Roaring stores a set of 32-bit integers (row numbers). It splits each integer into its high 16
bits, the key, and its low 16 bits. Each distinct key gets a container holding the low halves, and
the keys are kept in a sorted array beside their containers:

<figure class="diagram">
{{#include figures/bitmap-index-roaring.svg}}
<figcaption>A Roaring bitmap with three containers. Row 65,543 is <code>0x0001_0007</code>: its high half selects the container under key <code>0x0001</code>, and its low half is bit 7 of that bitmap container. The run container is serialised as one <code>(start, length - 1)</code> pair.</figcaption>
</figure>

There are three container types:

- **Array:** a sorted array of `u16`, for at most 4,096 values. At 4,096 values it takes 8,192
  bytes, the size of a bitmap, which is where the threshold comes from.
- **Bitmap:** 65,536 bits as 1,024 `u64` words, for more than 4,096 values.
- **Run:** a sorted list of `(start, length - 1)` pairs, used when it is smaller than either of the
  others. Serialised, it costs `2 + 4 · runs` bytes.

### Build the index for a segment

1. Scan the column. For each row `r` with value `v`, look up `v`'s bitmap in a hash map (creating it
   if needed) and append `r`. NULL rows go to the NULL bitmap.
2. Because `r` only grows, each append goes to the last container: add the low half to an array,
   or set a bit in a bitmap. When an array passes 4,096 values, convert it to a bitmap.
3. When the segment is complete, run-optimise every container: count its runs, and convert it to a
   run container if that is smallest.
4. Serialise each bitmap and store the bitmaps with a dictionary from value to bitmap offset.

### Insert, remove and test one value

1. Split `x` into `key = x >> 16` and `low = x & 0xFFFF`.
2. Binary-search the keys. If `key` is absent: a test answers "absent"; an insert creates a new
   array container at the right place; a remove does nothing.
3. Apply the operation to the container. Afterwards, normalise its type: an array that passes 4,096
   values becomes a bitmap, a bitmap that drops to 4,096 or fewer becomes an array, and an empty
   container is removed with its key.

### AND and OR

Both walk the two sorted key arrays together, like a merge:

1. **AND:** for keys present in both inputs, intersect the containers; keys present in only one
   input are dropped. Bitmap AND bitmap is a word-by-word `&` (count the result with `count_ones`
   and convert to an array if it has 4,096 values or fewer). Array AND bitmap tests each array value
   in the bitmap. Array AND array is a merge of two sorted lists, or a galloping search when one is
   much shorter. Drop containers that come out empty.
2. **OR:** keys from either input are kept; for keys in both, unite the containers. Array OR array
   is a sorted merge that becomes a bitmap if the result passes 4,096 values. Bitmap OR anything is
   a word-wise `|` or a series of bit sets.
3. **AND NOT** (`a` minus `b`) follows the AND walk, keeping `a`'s keys that `b` lacks unchanged.

<figure class="diagram">
{{#include figures/bitmap-index-and.svg}}
<figcaption>AND of two Roaring bitmaps. Keys <code>0x0000</code> and <code>0x0002</code> are in both inputs, so their containers are intersected: array AND array, and run AND array. Keys <code>0x0001</code> and <code>0x0003</code> are in only one input and are dropped without looking at their containers.</figcaption>
</figure>

Run containers add more pairs of cases. A simple approach is to convert a run container to a bitmap
for any operation you have not written a specialised version of, then normalise the result.

### NOT with NULLs

SQL's `NOT (color = 'R')` must not return NULL rows. Compute it as `ALL AND NOT (R OR NULL)`, where
`ALL` is the set of rows in the segment (minus deleted rows, if there is a [deletion
vector](deletion-vector.md)). Never complement against the full 32-bit range.

### Serialise

The [Roaring format specification](https://github.com/RoaringBitmap/RoaringFormatSpec) is the
interchange format, and Delta Lake and Iceberg [deletion vectors](deletion-vector.md) embed it.
Every number is little-endian:

1. A cookie. `12346` followed by a 32-bit container count if there are no run containers;
   otherwise `12347` in the low 16 bits with `count - 1` in the high 16 bits, followed by a bitset
   saying which containers are runs.
2. For each container: its key and `cardinality - 1`, both 16-bit.
3. An offset header with each container's 32-bit byte offset, present unless the cookie is `12347`
   and there are fewer than 4 containers.
4. The containers: an array as its `u16` values, a bitmap as 1,024 `u64` words, a run container as a
   16-bit run count followed by `(start, length - 1)` pairs.

A reader infers each non-run container's type from its cardinality: 4,096 or fewer is an array,
more is a bitmap.

### Variants

- **Range encoding.** Bitmap `i` marks the rows whose value is at most `vᵢ`, so a range predicate is
  one or two bitmaps instead of an `OR` over every value in the range (Chan and Ioannidis, 1998).
- **Bit-sliced index.** One bitmap per bit of a numeric value, so `k` distinct values need only
  about `log2(k)` bitmaps and sums can be computed from bitmap counts (O'Neil and Quass, 1997).
- **Word-aligned run-length codes.** WAH and Concise compress a bitmap into runs of words. They
  were the common choice before Roaring, which the Roaring papers compare against.
- **High cardinality.** With a bitmap per value and many values, most bitmaps hold one or two rows.
  That is an [inverted index](../specialised-indexes/inverted-index.md) with postings lists, which
  is the better structure there.

## Invariants

- Keys are strictly increasing, and no container is empty.
- An array container is strictly increasing and holds at most 4,096 values; a non-run container
  with more than 4,096 values is a bitmap.
- Run containers have sorted, non-overlapping, non-adjacent runs, and no run goes past 65,535.
- Every container's stored cardinality equals the number of values it holds, and the bitmap's
  cardinality is their sum.
- For a bitmap index, every row's bit is set in exactly one bitmap: its value's, or NULL's.

## Implementation pitfalls

- **Missing type conversions.** Because the format infers type from cardinality, serialising a
  bitmap container that has dropped to 4,096 values produces a file that readers parse as an array
  of 4,096 values from the wrong bytes. Normalise after every operation that can shrink or grow a
  container, including AND, AND NOT and remove.
- **Off-by-one fields.** Cardinality and run lengths are stored minus one, so a full container of
  65,536 values stores 65,535 and fits in 16 bits. Adding a run's start and length can still
  exceed 65,535 if the data is corrupt; use `checked_add` and reject it.
- **Counting runs in a bitmap.** A run starts at every set bit whose previous bit is clear. Per
  word, that is `(w & !((w << 1) | carry)).count_ones()`, where `carry` is the top bit of the
  previous word. Forgetting the carry counts a run that crosses a word boundary twice.
- **Bit and byte order.** Value `j` of a bitmap container is bit `j % 64` of word `j / 64`, and each
  word is written little-endian. Build words with `u64::from_le_bytes` on 8-byte chunks.
- **The offset header rule.** The offset header is absent only for run-container bitmaps with fewer
  than 4 containers. Getting this backwards shifts every container by a few bytes.
- **Empty bitmaps.** An empty bitmap is serialised with cookie `12346` and a count of 0. Make sure
  a reader accepts it.
- **More than 2^32 rows.** Row numbers above `u32::MAX` need the 64-bit extension: an array of
  32-bit keys, each with a 32-bit Roaring bitmap.
- **Indexing.** clippy denies `words[i]` in this crate. Iterate with `zip` over the two word
  arrays, and use `get` and `get_mut` with the `u16` index converted by `usize::from`.

## Testing

- Model-based: apply long random sequences of insert, remove, AND, OR and AND NOT to a Roaring
  bitmap and to a `BTreeSet<u32>` (or, for dense ranges, a `Vec<bool>`), and assert both hold the
  same values after each step. Check the invariants above after each step too.
- Thresholds: containers with exactly 4,095, 4,096 and 4,097 values, and operations that cross the
  threshold in each direction.
- Serialisation: round-trip random bitmaps, and parse the two files in the format specification's
  `testdata` directory, which the specification says every implementation should read.
- Edge values: 0, 65,535, 65,536, `u32::MAX`, a full container of 65,536 values, and an empty
  bitmap.
- Index results: for random columns with NULLs and random predicates, the rows from bitmap
  evaluation equal those of filtering the raw rows, `NOT` included.

## Further reading

- [Better bitmap performance with Roaring bitmaps](https://arxiv.org/abs/1402.6407), Chambi, Lemire,
  Kaser and Godin, 2016.
- [Consistently faster and smaller compressed bitmaps with
  Roaring](https://arxiv.org/abs/1603.06549), Lemire et al., 2016. Adds run containers.
- [roaringbitmap.org](https://roaringbitmap.org/), the format specification and implementations in
  many languages, including Rust.

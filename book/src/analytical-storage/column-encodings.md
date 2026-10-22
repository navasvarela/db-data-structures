# Column encodings

Lightweight encodings that make columns smaller and often faster to scan: dictionary encoding,
run-length encoding, delta encoding, frame-of-reference and bit-packing, and variable-length
integers.

- **Used by:** every columnar format; variable-length integers also appear in LevelDB, SQLite and
  Protocol Buffers records.
- **Notes:** a varint length prefix would be a natural first step for this crate's records, which
  use a fixed four-byte prefix today.

## How it works

### Layout of an encoded page

An encoding turns a sequence of values of one type into bytes, and back. A page records which
encoding it used and whatever parameters decoding needs, so every page can be decoded on its own:

```text
┌──────────────┬─────────────┬──────────────────────┬──────────────────────────┐
│ encoding id  │ value count │ parameters           │ encoded body             │
│ (1 byte)     │ (varint)    │ (bit width, minimum, │ (packed bits, runs, ...) │
│              │             │  dictionary ref, ...)│                          │
└──────────────┴─────────────┴──────────────────────┴──────────────────────────┘
```

Encodings compose. A dictionary turns strings into small integers; those integers are then
bit-packed or run-length encoded; the result may finally go through a general-purpose compressor
such as LZ4 or Zstandard. The sections below build up from the smallest piece.

### Variable-length integers (varints)

Small integers are common (lengths, counts, deltas), and a fixed eight bytes wastes most of them.
LEB128, the varint of Protocol Buffers, LevelDB and Parquet's headers, stores seven bits per byte
and uses the top bit to say "more bytes follow".

Encode an unsigned value `v`:

1. While `v >= 0x80`, emit `(v & 0x7F) | 0x80` and shift `v` right by 7.
2. Emit `v` as the last byte, with its top bit clear.

For example, 300 is `0b1_0010_1100`; the low seven bits give `0xAC` (with the continuation bit) and
the remaining `2` gives `0x02`, so 300 encodes as `AC 02`. A `u64` takes at most ten bytes.

Decode by accumulating `(byte & 0x7F) << shift`, adding 7 to `shift` after each byte, and stopping
at the first byte whose top bit is clear.

Signed values need one more step, or `-1` becomes ten bytes of ones. ZigZag encoding interleaves
negative and positive numbers (0, -1, 1, -2, 2 map to 0, 1, 2, 3, 4) before applying LEB128:

```text
encode: z = (n << 1) ^ (n >> 63)        (arithmetic shift of the signed n)
decode: n = (z >> 1) ^ -(z & 1)         (logical shift of the unsigned z)
```

SQLite uses a different varint (big-endian, at most nine bytes), so "varint" alone does not name
a format.

### Bit-packing

If every value in a page fits in `w` bits, store each in exactly `w` bits. The width is the
number of bits of the largest value:

```text
w = 64 - max.leading_zeros()          (0 when every value is 0)
bytes = ceil(n · w / 8)
```

Value `i` occupies bits `i·w .. (i+1)·w` of a little-endian bit stream: its least significant bit
goes to the lowest free bit. Packing `[5, 1, 7, 2]` with `w = 3` places them at bit offsets 0, 3,
6 and 9, which gives the 12-bit number `0x5CD`, stored as the bytes `CD 05`. This is the order of
Parquet's run-length/bit-packing hybrid. It lets a decoder load eight bytes into a `u64` and
extract values with a shift and a mask.

<figure class="diagram">
{{#include figures/column-encodings-bitpack.svg}}
<figcaption>Packing <code>[5, 1, 7, 2]</code> at <code>w = 3</code>, drawn with bit 0 on the right. Value <code>i</code> takes bits <code>3i</code> to <code>3i + 2</code>; the low byte is <code>CD</code> and the high byte <code>05</code>, with its top four bits as padding.</figcaption>
</figure>

Decode value `i`:

1. Compute its first bit `b = i · w`, the word `b / 64` and the offset `b % 64`.
2. Take `word >> offset`. If `offset + w > 64`, the value straddles two words: OR in the next word
   shifted left by `64 - offset`.
3. Mask with `(1 << w) - 1`, treating `w = 64` as "no mask".

### Frame of reference

Bit-packing alone does badly on values that are large but close together, such as Unix
timestamps from the same hour: each needs 31 bits, although they differ by less than 3,600.
Frame of reference (FOR) stores the page minimum once and bit-packs the differences from it:

1. Find `min` and `max` of the page.
2. Store `min` in the header, compute `w` from `max - min`.
3. Bit-pack `v - min` for every value.

Decoding adds `min` back. The subtraction must be done in the unsigned domain with wrapping
arithmetic, because `max - min` of two `i64`s can exceed `i64::MAX`.

### Delta encoding

Sorted or slowly changing columns (timestamps, auto-increment IDs) have small differences between
neighbours even when their range is huge. Delta encoding stores the first value, then the
differences, which FOR and bit-packing then shrink further.

Parquet's `DELTA_BINARY_PACKED` is a complete design to copy:

1. Header: block size in values (a multiple of 128), number of miniblocks per block (each
   miniblock a multiple of 32 values), total value count, and the first value as a ZigZag varint.
2. Compute the deltas between consecutive values.
3. For each block, find the minimum delta, subtract it from every delta in the block (so all are
   non-negative), and write the minimum delta as a ZigZag varint.
4. Write one byte per miniblock with its bit width, then each miniblock bit-packed at its own
   width.

For timestamps `1000, 1010, 1020, 1031` the deltas are `10, 10, 11`; after subtracting the minimum
delta 10 they are `0, 0, 1`, which pack into one bit each. A run of equal values packs to width 0,
so delta encoding also handles runs.

<figure class="diagram">
{{#include figures/column-encodings-delta.svg}}
<figcaption>Delta encoding of four timestamps. Subtracting the minimum delta leaves <code>0, 0, 1</code>, so the body is three bits; the first value, the minimum delta and the width go in the headers.</figcaption>
</figure>

### Run-length encoding

Run-length encoding (RLE) stores `(value, count)` pairs. It wins on sorted low-cardinality columns
and on validity streams, and loses on data that changes every row. Parquet's hybrid encoding
switches between runs and bit-packed groups inside one stream, using the low bit of each varint
header to tell them apart:

```text
RLE run:        varint(count << 1)             then the value in ceil(w / 8) bytes, little-endian
bit-packed run: varint((groups << 1) | 1)      then groups × 8 values packed at width w
```

So 100 copies of the value 4 at `w = 3` cost three bytes: `C8 01` (the varint of 200) and `04`.

Encode:

1. Scan the values, counting the length of the current run of equal values.
2. When a run reaches a threshold of your choice (8 is a natural one, the size of a bit-packed
   group), flush any pending bit-packed values and emit the run.
3. Otherwise add the values to a pending bit-packed run, in groups of 8. The last group is padded
   to 8 values; the decoder knows the real count from the page header.

### Dictionary encoding

A dictionary maps each distinct value to a small integer code, and the column stores codes:

1. Build a map from value to code while scanning the column chunk, assigning codes in order of
   first appearance.
2. Replace each value by its code; bit-pack or RLE the codes with `w` computed from the dictionary
   size.
3. Write the dictionary once per column chunk, before its data pages. Parquet stores it in a
   dictionary page encoded with the plain encoding.
4. If the dictionary grows past a size or entry limit, stop: encode the remaining pages without
   it. Parquet's specification describes exactly this fallback to plain encoding.

<figure class="diagram">
{{#include figures/column-encodings-dictionary.svg}}
<figcaption>Dictionary encoding of a city column, codes assigned in order of first appearance, then run-length encoding of the codes as <code>(value, count)</code> pairs. Eight strings become three dictionary entries and four runs.</figcaption>
</figure>

If the dictionary is sorted, codes preserve order. A predicate such as `city < 'M'` is then
evaluated once against the dictionary to find a code range, and the scan compares integers only.
The main store of an in-memory column store such as HYRISE keeps a sorted dictionary for this
reason (see [delta and main store](delta-and-main-store.md)).

### Choosing an encoding

A writer can pick per page from a few cheap statistics gathered while buffering: the number of
distinct values, the number of runs, `max - min`, and whether the values are sorted. A simple
policy:

1. Few distinct values: dictionary, with RLE or bit-packing of the codes.
2. Few runs relative to the value count: RLE.
3. Sorted or nearly sorted integers: delta.
4. Narrow range: frame of reference with bit-packing.
5. Otherwise: plain (values back to back), possibly followed by a general-purpose compressor.

A more thorough writer encodes a sample with each candidate and keeps the smallest.

### Operating on compressed data

Abadi, Madden and Ferreira showed that a query engine gains most when operators run on the
encoded form directly: a filter on a dictionary-encoded column compares codes, and `SUM` over an
RLE column multiplies each value by its run length instead of expanding the run.

## Invariants

- `decode(encode(values)) == values`, bit for bit, for every encoding and every input.
- Every bit-packed value is below `2^w`, and `w` is the smallest width that holds the largest
  value.
- RLE run lengths are at least 1, and they sum to the page's value count.
- Every dictionary code is below the dictionary size; the dictionary has no duplicates, and a
  sorted dictionary is strictly increasing.
- The value count in a page header equals the number of values the body decodes to.

## Implementation pitfalls

- **Bit-width off by one.** `ceil(log2(max))` gives 3 for `max = 8`, which needs 4 bits. Use
  `64 - max.leading_zeros()`, and handle width 0 (all zeros) without dividing or shifting by it.
- **Shifts of 64.** `1u64 << 64` overflows, and clippy's arithmetic lint denies it here. Worse,
  `1u64.wrapping_shl(64)` is `1`, not `0`, because `wrapping_shl` masks the shift amount. Use
  `checked_shl` and treat `None` as "all bits", or special-case `w == 64` when building masks.
- **Malformed varints.** A decoder fed bad input must reject a sequence with no terminating byte,
  more than ten bytes for a `u64`, or a tenth byte with bits above the 64th, instead of looping or
  overflowing.
- **ZigZag with the wrong shift.** `n >> 63` must be an arithmetic shift of the signed value. If
  `n` is converted to `u64` first, the shift becomes logical and negative numbers decode wrongly.
- **Delta overflow.** The delta between `i64::MIN` and `i64::MAX` overflows `i64`. Parquet
  requires wrapping two's-complement arithmetic so values are restored exactly; in Rust, compute
  in `u64` with `wrapping_sub` and `wrapping_add`.
- **Dictionary overflow fallback.** Once the dictionary passes its limit, earlier pages are already
  written with codes. The column chunk then mixes dictionary-encoded and plain pages, so the reader
  must look at each page's encoding, not only the first.
- **Padding read as data.** Bit-packed runs are padded to a group of 8 values, and Parquet tells
  readers to accept arbitrary padding bits. Stop at the value count from the header, never at the
  end of the bytes.
- **Floats as dictionary keys.** `NaN != NaN` and `-0.0 == 0.0`, so a dictionary keyed by `f64`
  equality loses values. Key floats by `to_bits()`.
- **Hot-loop indexing.** clippy denies `v[i]` in this crate. Iterate with `chunks_exact(8)` and
  `zip`, which also lets the compiler drop bounds checks.

## Testing

- Round-trip property tests for every encoding, with inputs drawn from several distributions: all
  equal, strictly increasing, random in a narrow range, random over the full `i64` range, and
  alternating `i64::MIN` and `i64::MAX`.
- Exhaustive widths: bit-pack and unpack random values at every width from 0 to 64, with value
  counts that are and are not multiples of 8 and 64.
- Known bytes: assert that 300 encodes as `AC 02`, that `[5, 1, 7, 2]` packs to `CD 05` at width
  3, and that the examples in the Parquet encodings document encode as written there.
- Size expectations: a constant column encodes to a few bytes with RLE or delta, independent of
  its length.
- Decoder robustness: feed random bytes to every decoder; each must return an error or some
  values, and never panic.

## Further reading

- [Integrating Compression and Execution in Column-Oriented Database
  Systems](https://www.cs.umd.edu/~abadi/papers/abadisigmod06.pdf), Abadi, Madden and Ferreira,
  2006. Operating directly on compressed data.
- [Decoding billions of integers per second through vectorization](https://arxiv.org/abs/1209.2137),
  Lemire and Boytsov, 2015. Bit-packing and frame-of-reference done fast.
- [The FastLanes Compression Layout](https://www.vldb.org/pvldb/vol16/p2132-afroozeh.pdf), Afroozeh
  and Boncz, 2023.
- [Parquet encodings](https://parquet.apache.org/docs/file-format/data-pages/encodings/). The
  dictionary, RLE/bit-packing hybrid and delta encodings of a real format.

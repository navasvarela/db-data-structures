# Floating-point and string compression

> Builds on [column encodings](../analytical-storage/column-encodings.md).

Integer encodings do little for floats and strings, which have their own schemes:

- **Gorilla:** stores each float as the XOR with the previous one, which is mostly zero bits for a
  slowly changing series, and timestamps as deltas of deltas.
- **ALP:** turns doubles that came from decimals back into integers by a shared power of ten, then
  bit-packs them, with exceptions for values that do not round-trip.
- **FSST:** replaces frequent substrings of up to eight bytes with one-byte codes from a table of
  255 symbols, while still allowing a single string to be decoded on its own.

- **Used by:** Gorilla in Prometheus and InfluxDB time-series storage; ALP and FSST in DuckDB.
- **Notes:** Pelkonen et al., 2015 (Gorilla); Afroozeh, Kuffó and Boncz, 2023 (ALP); Boncz, Neumann
  and Leis, 2020 (FSST).

## How it works

### Why integer encodings fail here

An IEEE 754 double is a sign bit, 11 exponent bits and 52 mantissa bits. Most decimal values, such
as `0.1`, have no exact binary form, so the low mantissa bits look random even when two values are
close: only the high bits (sign, exponent, first mantissa bits) repeat. Treating the 64-bit
patterns as integers and applying delta or frame-of-reference encoding therefore saves little.
Strings have a different problem: each one is short and variable-length, so a block compressor
such as LZ4 would have to decompress a whole block to read one value. Gorilla and ALP attack the
first problem in two different ways; FSST attacks the second.

### Gorilla: layout

Gorilla compresses one time series at a time into blocks, each covering a two-hour window. A block
is a bit stream in which timestamps and values alternate:

```text
┌──────────────────┬────────────┬──────────┬──────┬───────┬──────┬───────┬─────
│ t₋₁: window start│ Δt₀        │ v₀       │ t₁   │ v₁    │ t₂   │ v₂    │ ...
│ (header)         │ (14 bits)  │ (64 bits)│ ΔΔ   │ XOR   │ ΔΔ   │ XOR   │
└──────────────────┴────────────┴──────────┴──────┴───────┴──────┴───────┴─────
```

The header holds the window's aligned start time `t₋₁`. The first timestamp is stored as a 14-bit
delta from it (enough for a bit more than four hours of seconds), and the first value is stored
uncompressed.

### Gorilla: encode a timestamp

For each later timestamp `tₙ`:

1. Compute the delta of deltas `D = (tₙ - tₙ₋₁) - (tₙ₋₁ - tₙ₋₂)`. Points that arrive at a fixed
   interval give `D = 0`.
2. Write the shortest bucket that fits:

| `D` | Written |
| --- | --- |
| 0 | `0` |
| -63 to 64 | `10` + 7 bits |
| -255 to 256 | `110` + 9 bits |
| -2047 to 2048 | `1110` + 12 bits |
| anything else | `1111` + 32 bits |

The paper reports that about 96% of Facebook's timestamps compressed to the single `0` bit.

### Gorilla: encode a value

For each later value, XOR its bit pattern with the previous value's. Close values share sign,
exponent and leading mantissa bits, so the XOR has many leading and trailing zeros.

1. If the XOR is zero (the same value), write `0`.
2. Otherwise write `1`, count the XOR's leading and trailing zeros, and then:
   1. If the meaningful bits fit inside the previous window (at least as many leading and as many
      trailing zeros as the window), write `0` and the bits inside that window.
   2. Otherwise write `1`, the number of leading zeros in 5 bits, the number of meaningful bits in
      6 bits, then the meaningful bits. This sets a new window.

<figure class="diagram">
{{#include figures/floating-point-and-string-compression-gorilla.svg}}
<figcaption>Encoding the values 15.0, 15.0, 12.0 and 14.0. The repeat costs the single bit <code>0</code>. The XOR of 15.0 and 12.0 has 13 leading zeros and 2 meaningful bits, so it writes <code>11</code>, 13 in 5 bits, 2 in 6 bits and the bits themselves, which sets the window (blue). The next XOR fits inside that window and costs <code>10</code> plus the 2 window bits.</figcaption>
</figure>

Decoding reverses each step, keeping the previous timestamp, previous delta, previous value and
current window as state. The stream cannot be entered in the middle: reading point `n` means
decoding points `0..n`, which is why blocks are kept short.

### ALP: layout

ALP (adaptive lossless floating-point) targets doubles that were decimals before they became
binary, such as prices and sensor readings. It works on vectors of up to 1,024 values. Parquet
adopted it as the `ALP` encoding, in preview at the time of writing, and its specification gives a
concrete vector layout:

```text
┌──────────────┬──────────┬──────────────────┬───────────────┬───────────────────┐
│ e, f         │ FOR base,│ bit-packed       │ exception     │ exception values  │
│ exception    │ bit width│ deltas           │ positions     │ (raw IEEE 754     │
│ count        │          │                  │ (u16 each)    │  bit patterns)    │
└──────────────┴──────────┴──────────────────┴───────────────┴───────────────────┘
```

### ALP: encode a vector

1. Choose an exponent `e` and a factor `f` with `0 <= f <= e` (`e <= 18` for doubles). Trying all
   pairs on every vector is slow; the paper samples a few vectors per row group to keep the best
   handful of pairs (up to 5), then tries only those on each vector, using a sample of its values.
2. For each value `v`, compute the integer `d = round(v · 10^e · 10^-f)`.
3. Check the round trip: decode `d` as `d · 10^f · 10^-e` and compare bit patterns with `v`. If they
   differ, or `v` is NaN, infinite, `-0.0`, or out of the `i64` range, `v` is an exception: record
   its position and raw bits, and put a placeholder (the encoded integer of some non-exception
   value) in its slot.
4. Frame of reference: subtract the minimum `d` from every slot and bit-pack the results (see
   [column encodings](column-encodings.md)).
5. Write the parameters, the packed integers and the exceptions.

### ALP: decode a vector

1. Unpack the integers and add the frame-of-reference base back.
2. Convert each to a double and multiply by `10^f`, then by `10^-e`, exactly as the encoder's
   check did.
3. Overwrite the exception positions with their stored bit patterns.

Because every value either round-trips through steps 1 and 2 or is stored as an exception, the
result is lossless however `e` and `f` were chosen; a poor choice only costs space. The paper also
describes ALP_RD for high-precision doubles that are not decimals: it splits each bit pattern into
a left part, which is dictionary-encoded, and a right part, which is stored as is.

### FSST: layout

FSST (fast static symbol table) compresses each string independently using a table of up to 255
symbols, each 1 to 8 bytes long. Every code is one byte; code 255 is the escape, meaning "the next
byte is a literal".

<figure class="diagram">
{{#include figures/floating-point-and-string-compression-fsst.svg}}
<figcaption>Compressing <code>"http://www.example.com"</code> with a four-symbol table. Each position takes the longest matching symbol; letters with no symbol are escaped as code 255 followed by the literal byte. The 22 input bytes become 15 codes and literals.</figcaption>
</figure>

The table is the only state needed to decode, and it is at most `8 · 255 + 255` bytes, so it is
stored once per block, page or segment.

### FSST: compress a string

With a symbol table built (next section):

1. At the current position, find the longest symbol that matches the input.
2. If one matches, emit its code and advance by its length.
3. Otherwise emit 255 followed by the input byte, and advance by one.

A string never grows by more than a factor of two, in the case where every byte is escaped.

### FSST: decompress a string

For each code: if it is 255, copy the next byte; otherwise append the code's symbol. The paper's
implementation stores each symbol as an 8-byte word and writes all 8 bytes unconditionally, then
advances the output by the symbol's real length, which needs no branch on the length.

### FSST: build the symbol table

The table is built bottom-up over a sample, in a few rounds ("generations"):

1. Start with an empty table, in which every byte is escaped.
2. Compress the sample with the current table, counting how often each code occurs and how often
   each pair of consecutive codes occurs.
3. Candidates are every current symbol, every single byte, and every concatenation of two
   consecutive symbols (cut to 8 bytes). Score each by its gain, `length · count`.
4. Keep the 255 candidates with the highest gain as the new table.
5. Repeat from step 2. The paper uses five generations, which is enough for symbols to grow to 8
   bytes, and trains on a 16 KB sample for each 4 MB of string data.

Two strings compressed with the same table are equal exactly when their compressed forms are
equal, so an equality predicate can compress its constant once and compare compressed bytes. Order
is not preserved, so range predicates still need decompression.

### Variants

- **Byte stream split.** Parquet's `BYTE_STREAM_SPLIT` writes byte 0 of every value, then byte 1
  of every value, and so on. It does not shrink the data itself, but it groups the predictable
  exponent bytes together so a general-purpose compressor that runs afterwards does better.
- **Delta-of-delta for any integer timestamp column**, outside Gorilla, follows the same idea as
  delta encoding applied twice.

## Invariants

- Every scheme is lossless bit for bit: decoding reproduces the exact IEEE 754 bit patterns
  (including `-0.0` and NaN payloads) and the exact bytes of each string.
- ALP: every exception position is below the vector length, positions are distinct, and every
  non-exception slot decodes to its original value under the decoder's formula.
- FSST: every code below 255 is less than the table's symbol count, every 255 is followed by a
  byte, and every symbol is 1 to 8 bytes long.
- Gorilla: the number of points decoded equals the number written.

## Implementation pitfalls

- **Comparing floats with `==`.** `-0.0 == 0.0` is true and `NaN == NaN` is false, so a round-trip
  check with `==` both accepts a lossy result and rejects a correct one. Compare `to_bits()`.
- **Gorilla's 6-bit length field.** The number of meaningful bits can be 64 (no leading or trailing
  zeros), which does not fit in 6 bits. Prometheus writes 0 for 64, which is safe because 0
  meaningful bits never occurs in that branch. It also clamps a leading-zero count above 31 to 31
  so it fits in 5 bits.
- **Gorilla's asymmetric buckets.** `-63..=64` is not the two's-complement range of 7 bits
  (`-64..=63`). Encoder and decoder must agree on the mapping; Prometheus decodes a field as
  negative only when it exceeds `2^(n-1)`.
- **End of a bit stream.** The last byte is padded with zero bits, which can decode as extra
  points. Store the point count in the header and stop there.
- **ALP's powers of ten.** Parquet's specification requires the exact, correctly rounded constants
  `1e0` to `1e18` and `1e-1` to `1e-18`, computed as two separate multiplications, and forbids
  computing them at runtime with `pow`. Keep them in a literal table, or two implementations will
  disagree in the last bit.
- **Float-to-integer casts.** In Rust, `f64 as i64` saturates and turns NaN into 0 instead of
  failing, so an out-of-range value is silently mangled. Check the range before converting, and
  make such values exceptions.
- **Unpatched exception slots.** If an exception's slot keeps a garbage integer instead of a
  placeholder, the frame-of-reference range, and therefore the bit width, explodes for the whole
  vector.
- **FSST's 8-byte store.** Writing a full 8-byte symbol needs 7 bytes of slack after the output's
  end. Without `unsafe`, either reserve slack and truncate at the end, or copy only the symbol's
  real length with `extend_from_slice`.
- **FSST and byte 255.** The byte 255 in the input is never confused with the escape, because it is
  itself escaped (or part of a symbol). Test strings made of 255s and empty strings explicitly.
- **Comparing compressed strings from different tables.** Compressed equality only holds within
  one symbol table. Comparing across segments needs decompression or re-encoding.

## Testing

- Round trip on random bit patterns: build doubles with `f64::from_bits` from random `u64`s, which
  covers NaNs, infinities, subnormals and `-0.0`, and compare bit patterns after decoding.
- Round trip on realistic data: random integers divided by 100 (prices), slowly drifting sensor
  values, and constant series. ALP should report few exceptions on the first; Gorilla should spend
  about one bit per point on a constant series.
- Timestamps at exact intervals, with jitter, with gaps, and with deltas on either side of
  each bucket boundary.
- FSST on random bytes, on empty strings, on strings of byte 255, on one string repeated many
  times, and on a corpus of URLs; check that compressed equality agrees with raw equality.
- Cross-check ALP against the vector layout in Parquet's ALP specification if you follow it.

## Further reading

- [Gorilla: A Fast, Scalable, In-Memory Time Series
  Database](https://www.vldb.org/pvldb/vol8/p1816-teller.pdf), Pelkonen et al., 2015. Section 4.1
  describes the timestamp and value compression.
- [ALP: Adaptive Lossless floating-Point Compression](https://doi.org/10.1145/3626717), Afroozeh,
  Kuffó and Boncz, 2023, with a [reference implementation](https://github.com/cwida/ALP).
- [FSST: Fast Random Access String Compression](https://www.vldb.org/pvldb/vol13/p2649-boncz.pdf),
  Boncz, Neumann and Leis, 2020.

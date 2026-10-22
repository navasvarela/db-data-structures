# Bloom filter

A bit array with `k` hash functions that answers "definitely absent" or "maybe present". It has no
false negatives, so a "no" lets a reader skip a disk read with certainty.

- **Used by:** RocksDB, LevelDB and Cassandra per SSTable; join filters in query engines.
- **Cost:** about 9.6 bits per key for a 1% false-positive rate, with `k = 7`. No deletes.
- **Notes:** Bloom, 1970. Dayan et al., 2017 ("Monkey") show how to size filters per LSM level.

## How it works

### Layout

A Bloom filter is an array of `m` bits, all zero at first, and a way to derive `k` bit positions
from a key. It stores nothing else: no keys, no counts. The figures below use 16 bits and `k = 3`.

Two numbers are chosen up front from the expected number of keys `n` and the target
false-positive rate `p`:

- bits: `m = -n · ln(p) / (ln 2)²`, which is about 9.6 bits per key for `p = 1%`;
- probes: `k = (m / n) · ln 2`, rounded to the nearest integer, which is 7 for `p = 1%`.

With those values, the probability that a key never inserted finds all its bits set is about
`(1 - e^(-k·n/m))^k`.

### Deriving the `k` positions

Computing `k` independent hashes per key is slow, and unnecessary. Kirsch and Mitzenmacher showed
that two hashes are enough: compute one 64-bit hash, split it into halves `h1` and `h2`, and use

```text
position_i = (h1 + i · h2) mod m        for i in 0..k
```

LevelDB does the same with a single 32-bit hash, using a rotation of it as `h2`.

### Insert

1. Hash the key once to get `h1` and `h2`.
2. For each `i` from 0 to `k - 1`, compute `position_i` and set that bit to 1.

<figure class="diagram">
{{#include figures/bloom-filter-insert.svg}}
<figcaption>Inserting two keys with k = 3. Bit 9 is shared: setting a bit that is already 1 changes nothing.</figcaption>
</figure>

### Query

1. Hash the key and compute the same `k` positions.
2. If any of those bits is 0, the key was never inserted: answer "definitely absent".
3. If all are 1, answer "maybe present". Either the key was inserted, or other keys happened to set
   all of its bits.

A query can stop at the first zero bit, so absent keys, the common case in an LSM read, are often
answered after one or two probes.

<figure class="diagram">
{{#include figures/bloom-filter-query.svg}}
<figcaption>Two queries against the filter above. The dashed probes are never made, because the first one already found a zero.</figcaption>
</figure>

### Building one per SSTable

In an LSM engine the key count is known when an SSTable is written, so the filter is sized
exactly:

1. While writing the table, collect the hash of every key (not the keys themselves).
2. When the table is finished, size the filter from the number of hashes, set the bits, and write
   the bytes into a filter block.
3. Store `k` next to the bits, so a reader does not have to know the writer's settings. LevelDB
   stores it in the filter's last byte.
4. On open, load the filter block into memory and check it before reading any data block.

### Variants

- **Blocked Bloom filter:** the first hash picks one 512-bit block, the size of a cache line, and
  all `k` probes land inside it. A query costs one cache miss instead of up to `k`, for a slightly
  higher false-positive rate at the same size. RocksDB's current filters use this layout.
- **Counting Bloom filter:** small counters instead of bits, so a key can be removed by
  decrementing them. It costs about four times the space; a [cuckoo filter](cuckoo-filter.md) is
  usually the better way to get deletes.

## Invariants

- Bits are only ever set, never cleared. This is what guarantees there are no false negatives.
- After a key is inserted, all `k` of its positions are 1.
- `m`, `k` and the hash function are fixed for the lifetime of a filter, and are part of its
  serialised form.
- Two filters with the same `m`, `k` and hash function can be merged by OR-ing their bits; the
  result is the filter of the union of their keys.

## Implementation pitfalls

- **Unstable hashing.** Rust's `DefaultHasher` and `RandomState` are seeded per process, and their
  algorithm may change between Rust versions. A filter written with them cannot be read after a
  restart. Use a hash with a fixed specification and seed for anything that reaches disk.
- **`h2 = 0`.** If the second hash is zero, every probe hits the same bit, and the filter degrades
  to `k = 1`. Force `h2` to be odd, or otherwise non-zero.
- **Sizing with zero keys.** An empty table gives `m = 0` and a division by zero in the
  position calculation. Clamp `m` to a minimum (LevelDB uses 64 bits) and `k` to a range such as
  1 to 30.
- **Rounding `m`.** Bits are stored in bytes or 64-bit words, so `m` gets rounded up. Use the
  rounded value consistently when writing and reading, or the positions disagree.
- **Slow modulo.** `hash % m` is a division per probe. Lemire's multiply-shift reduction,
  `(hash · m) >> 64` computed in 128 bits, maps a 64-bit hash onto `0..m` without dividing. In
  this crate, clippy denies plain arithmetic that can overflow, so the multiplication has to be
  written as a widening one.
- **Measuring the false-positive rate on the wrong keys.** It is only meaningful on keys that were
  never inserted.

## Testing

- No false negatives: insert a random set of keys and assert every one of them answers "maybe
  present". A property test over many random sets catches probe-calculation bugs quickly.
- False-positive rate: query 100,000 keys that were never inserted, and check that the measured
  rate is close to the formula above, for example within 1.5 times.
- Serialisation: write a filter to bytes, read it back in a fresh process or with a fresh hasher,
  and repeat both checks.
- Edge cases: an empty filter, a filter of one key, and `k` at both ends of its allowed range.

## Further reading

- [Space/Time Trade-offs in Hash Coding with Allowable
  Errors](https://doi.org/10.1145/362686.362692), Bloom, 1970.
- [Network Applications of Bloom Filters: A
  Survey](https://www.eecs.harvard.edu/~michaelm/postscripts/im2005b.pdf), Broder and Mitzenmacher,
  2004. The clearest derivation of the false-positive rate and the optimal number of hashes.
- [Less Hashing, Same Performance: Building a Better Bloom
  Filter](https://www.eecs.harvard.edu/~michaelm/postscripts/rsa2008.pdf), Kirsch and Mitzenmacher.
  Two hash functions are enough to simulate `k`.
- [Monkey: Optimal Navigable Key-Value
  Store](https://cs-people.bu.edu/mathan/publications/sigmod17-dayan.pdf), Dayan, Athanassoulis and
  Idreos, 2017. Sizing filters per LSM level.

# Range filter

Answers "might any key in `[a, b]` exist?", which a point filter cannot. It lets an LSM range scan
skip SSTables, as a Bloom filter lets a point lookup skip them.

- **Used by:** RocksDB's prefix Bloom filters cover the special case of scans over a fixed key
  prefix.
- **Notes:** SuRF (Zhang et al., 2018) truncates a succinct trie of the keys; Rosetta (Luo et al.,
  2020) stacks Bloom filters over key prefixes of increasing length.

## How it works

A [Bloom filter](bloom-filter.md) hashes keys, which destroys their order: nothing in the bits
says whether a key between `a` and `b` exists. Probing every possible key in the range is only
feasible for tiny integer ranges. Range filters keep some ordering information instead. There are
three practical designs, from simplest to most general.

### Prefix Bloom filter

If every scan stays inside one fixed-length key prefix (for example "all events of user 42", with
the user id as the first 8 bytes), the prefix itself can be the filter key:

1. When writing an SSTable, insert the prefix of every key into an ordinary Bloom filter.
2. Before a scan over a range that lies entirely inside one prefix, probe the filter with that
   prefix. A "definitely absent" skips the table.

RocksDB does this when `prefix_extractor` is set. The design fails as soon as a range crosses a
prefix boundary, and it needs the prefix length to be chosen in advance.

### SuRF: a truncated succinct trie

SuRF stores the keys in a trie (a tree whose edges are key bytes, so the keys of a subtree share
a prefix) and then cuts it down until it is small enough to be a filter.

**Truncation.** SuRF-Base keeps, for each key, only the shortest prefix that distinguishes it
from every other key: the shared prefix plus one more byte.

<figure class="diagram">
{{#include figures/range-filter-surf-trie.svg}}
<figcaption>The keys <code>SIGAI</code>, <code>SIGMOD</code> and <code>SIGOPS</code> share the prefix <code>SIG</code>, so SuRF-Base keeps one byte more of each. The leaf <code>SIGA</code> now stands for every key starting <code>SIGA</code>.</figcaption>
</figure>

A leaf now stands for "some key starting with this prefix". Point and range answers can be wrong
in one direction only: false positives, never false negatives.

**Encoding.** The trie is stored as a Fast Succinct Trie, in level order (breadth first):

- **LOUDS-Dense** for the few upper levels: each node is three bitmaps, 256 bits marking which
  byte labels exist, 256 bits marking which labels lead to a child, and one bit saying whether the
  prefix ending at this node is itself a key.
- **LOUDS-Sparse** for the many lower levels: a byte array of labels, one has-child bit per label,
  and one bit per label marking the first label of each node.

Moving from a label to its child, or back to the parent, is a `rank` or `select` on these bit
arrays (`rank(i)` counts the 1 bits before position `i`; `select(j)` finds the `j`-th 1 bit). The
dense part is used while its size is less than 1/64 of the sparse part (the paper's ratio
`R = 64`). LOUDS-Sparse costs about 10 bits per trie node, and SuRF-Base measured about 10 bits
per key on random 64-bit integers and 14 on email addresses.

**Suffix bits.** Variants store a few extra bits at each leaf to cut false positives:

- **SuRF-Hash:** `n` bits of a hash of the full key. Point false positives drop below `2^-n`, but
  hash bits carry no order, so range queries do not improve.
- **SuRF-Real:** the next `n` bits of the real key after the truncated prefix. These improve both
  point and range queries, but less for point queries than hash bits do.
- **SuRF-Mixed:** some of each.

**Range query.** To test `[a, b]`:

1. Seek to the smallest stored prefix `p` that could hold a key `≥ a` (a lower-bound search
   through the trie, moving to the next label when a byte of `a` is missing).
2. If there is no such prefix, answer "definitely absent".
3. If `p` (with its real suffix bits, if stored) is greater than `b` and is not a prefix of `b`,
   every key it stands for is greater than `b`: answer "definitely absent".
4. Otherwise answer "maybe present".

If `a` itself matches `p` as far as `p` goes, SuRF cannot tell whether the hidden key is below or
above `a`, so it must assume above. That is the source of range false positives.

<figure class="diagram">
{{#include figures/range-filter-surf-query.svg}}
<figcaption>Two range queries against the SuRF-Base trie above. For <code>[SIGB, SIGL]</code> the lower-bound search finds <code>SIGM</code>, which is greater than <code>SIGL</code>: definitely absent. For <code>[SIGMP, SIGN]</code> it finds <code>SIGM</code>, which matches <code>SIGMP</code> as far as it goes, so the answer is "maybe present". The hidden key <code>SIGMOD</code> is below <code>SIGMP</code>, so this is a false positive.</figcaption>
</figure>

### Rosetta: Bloom filters over every prefix length

Rosetta treats keys as fixed-width `L`-bit integers and keeps `L` Bloom filters, one per prefix
length. Filter `l` holds the top `l` bits of every key. Together they describe an implicit
segment tree: a prefix of length `l` is a node covering `2^(L - l)` consecutive keys.

<figure class="diagram">
{{#include figures/range-filter-rosetta.svg}}
<figcaption>The implicit segment tree for <code>L = 4</code> and stored keys <code>0101</code> and <code>1100</code>. Level <code>l</code> is a Bloom filter holding the bold prefixes of that length. The highlighted nodes are the dyadic intervals of the query <code>[3, 12]</code>, explained below.</figcaption>
</figure>

To test `[a, b]`:

1. Split the range into **dyadic intervals**: maximal aligned blocks of size `2^j`, each of which
   is exactly one prefix. `[3, 12]` with `L = 4` becomes `[3]`, `[4, 7]`, `[8, 11]`, `[12]`, that
   is prefixes `0011`, `01`, `10` and `1100`. A range of length `R` needs about `2 · log2(R)`
   intervals at most.
2. Probe each interval's prefix in the Bloom filter of its length. If all say "absent", the range
   is empty.
3. A positive probe on a short prefix may be a false positive. Rosetta checks it by probing the
   two children (one bit longer) in the next filter down, recursing towards full-length keys, and
   only answers "maybe present" if some path stays positive to the bottom.

Rosetta is strongest for short ranges, where few probes are needed and the bottom filter makes
point queries accurate. Long ranges need many probes. The paper also tunes how many levels to keep
and how to split memory between them for each LSM run.

### Variants

Later designs such as bloomRF and Proteus refine these ideas. Proteus combines a truncated trie
with a prefix Bloom filter and tunes the mix from a sample of queries.

## Invariants

- No false negatives: if any stored key `k` satisfies `a ≤ k ≤ b`, the filter answers "maybe
  present". Point queries are the special case `a = b`.
- The filter compares keys in exactly the order the SSTable sorts them.
- SuRF: labels within a node are in ascending order, every stored key has exactly one leaf, and
  that leaf's prefix is a prefix of the key.
- Rosetta: for every stored key and every `l` in `1..=L`, the key's `l`-bit prefix answers
  "maybe present" in filter `l`.
- The dyadic intervals of `[a, b]` are disjoint, aligned, and their union is exactly `[a, b]`.

## Implementation pitfalls

- **Order mismatch.** The filter must order keys exactly as the table does. For Rosetta, encode
  integers big-endian and flip the sign bit of signed integers, so that bit order matches numeric
  order. For SuRF, compare bytes unsigned.
- **Keys that are prefixes of other keys.** With byte-string keys, `"ab"` and `"abc"` can both be
  stored. SuRF marks such nodes (the LOUDS-Dense prefix-key bit, or a reserved first label in
  LOUDS-Sparse). Forgetting it loses `"ab"` and creates false negatives.
- **Treating "matches the prefix of `a`" as "less than `a`".** When `a` agrees with a truncated key
  as far as it is stored, the answer must be "maybe present". Skipping that key is a false
  negative.
- **Bound conventions.** Decide whether ranges are inclusive or half-open, and convert once at the
  API boundary. Handle `a > b` as an empty range.
- **Overflow at the top of the domain.** Dyadic decomposition often computes `b + 1` or
  `a + 2^j`. At `u64::MAX` these overflow; this crate's clippy settings deny unchecked arithmetic,
  so use `checked_add` and treat overflow as reaching the end of the domain.
- **Long ranges in Rosetta.** The number of probes grows with the range length, and recursive
  checking multiplies it. Cap the work and answer "maybe present" when the cap is hit; that is
  always safe.
- **Hash suffixes on range queries.** SuRF-Hash bits must be ignored on range queries, because a
  hash says nothing about order.
- **Prefix filters on crossing ranges.** A prefix Bloom filter may only be consulted when both ends
  of the scan share the prefix. Using it on a range that crosses into another prefix produces false
  negatives.
- **Slow `rank` and `select`.** Counting bits from the start of the array on every step makes a
  lookup linear in the trie size. Precompute rank counts per block of, say, 512 bits, and sample
  the positions of every few hundred 1 bits for `select`.

## Testing

- Oracle: keep the keys in a `BTreeSet` and compare every answer with
  `set.range(a..=b).next().is_some()`. A "definitely absent" when the oracle finds a key is a bug.
- False-positive rate: measure it separately for empty ranges of several lengths (1, 16, 1,000
  keys wide) and for two placements: uniformly random, and ranges that start immediately after a
  stored key. Truncation-based filters do much worse on the second kind, so test it explicitly.
- Dyadic decomposition: a property test that the intervals are disjoint, aligned and cover
  exactly `[a, b]`, including at `0` and the maximum key.
- Edge cases: an empty filter, one key, `a = b`, ranges at the minimum and maximum keys, the empty
  byte string, keys that are prefixes of other keys, and keys containing `0x00` and `0xFF` bytes.
- Serialisation: write the filter, read it back, and rerun the oracle comparison.

## Further reading

- [SuRF: Practical Range Query Filtering with Fast Succinct
  Tries](https://www.pdl.cmu.edu/PDL-FTP/Storage/surf_sigmod18.pdf), Zhang et al., 2018.
- [Rosetta: A Robust Space-Time Optimized Range Filter for Key-Value
  Stores](https://doi.org/10.1145/3318464.3389731), Luo et al., 2020.

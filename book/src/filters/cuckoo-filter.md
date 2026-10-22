# Cuckoo filter

A table of short key fingerprints placed by cuckoo hashing. Like a Bloom filter it answers
membership approximately, but it also supports deletion.

- **Cost:** smaller than a Bloom filter for false-positive rates below about 3% with the paper's
  semi-sorting optimisation, and below about 0.4% without it.
- **Notes:** Fan et al., 2014.

## How it works

### Layout

A cuckoo filter is an array of `m` buckets, each holding `b` slots. A slot holds an `f`-bit
**fingerprint** of a key (a few bits taken from its hash) or is empty. The keys themselves are not
stored. The paper's default, and the usual choice, is `b = 4`. The figures below use `m = 8`
buckets and 8-bit fingerprints written in hex.

Every key has exactly two candidate buckets, and its fingerprint lives in one of them. A lookup
therefore reads at most two buckets, `2b` slots, whatever the table size.

<figure class="diagram">
{{#include figures/cuckoo-filter-layout.svg}}
<figcaption>The key <code>"apple"</code> hashes to fingerprint <code>5a</code> and bucket <code>i1 = 2</code>. Its alternate bucket is <code>i1 xor hash(5a)</code>; here <code>hash(5a) mod 8 = 7</code>, so <code>i2 = 5</code>. A lookup reads only the two outlined buckets.</figcaption>
</figure>

### Partial-key cuckoo hashing

Classic cuckoo hashing moves a key to its other bucket by rehashing the key. A filter has only the
fingerprint, so the alternate bucket must be computable from the fingerprint and the current
bucket alone:

```text
i1 = hash(key) mod m
i2 = i1 xor (hash(fingerprint) mod m)
```

Xor is its own inverse, so from either bucket `i` the other one is `i xor hash(fingerprint)`. The
fingerprint is hashed before the xor; xoring the raw 8-bit fingerprint would only change the low
eight bits of the index, so displaced items would stay within 256 buckets of where they started.

With xor, `m` must be a power of two so that the result stays inside the table. If `m` cannot be a
power of two, `j = (hash(fingerprint) - i) mod m` is another involution that works for any `m`.

### Insert

1. Hash the key once. Take the bucket index `i1` from one part of the hash and the fingerprint `fp`
   from another, so the two are independent. If `fp` is 0, map it to 1, because 0 marks an empty
   slot.
2. Compute `i2`. If either bucket has an empty slot, store `fp` there and stop.
3. Otherwise pick `i1` or `i2` at random as the current bucket `i`, and repeat up to
   `MAX_KICKS` times (the paper uses 500):
   1. Pick a random slot in bucket `i`, swap `fp` with the fingerprint stored there.
   2. Set `i` to the alternate bucket of the fingerprint now in hand: `i xor hash(fp)`.
   3. If bucket `i` has an empty slot, store `fp` there and stop.
4. If the loop ends, the table is considered full. One fingerprint, which may belong to a key
   inserted long ago, is still in hand; see the pitfalls below.

<figure class="diagram">
{{#include figures/cuckoo-filter-kick.svg}}
<figcaption>Inserting <code>"fig"</code>, fingerprint <code>3c</code>, into a fuller table. Both candidate buckets, 1 and 4, are full, so <code>3c</code> takes the slot of <code>09</code> in bucket 4. <code>09</code> moves to its other bucket, <code>4 xor hash(09) = 6</code>, where it evicts <code>d0</code>; <code>d0</code> moves to <code>6 xor hash(d0) = 5</code>, which has a free slot.</figcaption>
</figure>

### Lookup

1. Compute `fp`, `i1` and `i2` as for insert.
2. Answer "maybe present" if either bucket contains `fp`, otherwise "definitely absent".

### Delete

1. Compute `fp`, `i1` and `i2`.
2. If either bucket contains `fp`, clear one copy of it and report success.

Deleting removes one copy only. If two keys share a fingerprint and a bucket, it does not matter
which copy goes: the other key still finds the remaining one. Deletion is only safe for keys that
were inserted. Deleting a key that was never inserted may remove another key's fingerprint and
create a false negative.

### Sizing and error bounds

A lookup compares `fp` with up to `2b` stored fingerprints, each matching by chance with
probability `1 / 2^f`. The false-positive rate is therefore at most

```text
ε ≤ 1 - (1 - 1/2^f)^(2b)  ≈  2b / 2^f
```

which gives the fingerprint size `f ≥ log2(1/ε) + log2(2b)`, that is `log2(1/ε) + 3` bits for
`b = 4`. The table cannot be filled completely: insertions start failing at a load factor `α` of
about 50% for `b = 1`, 84% for `b = 2`, 95% for `b = 4` and 98% for `b = 8`. The cost per key is

```text
bits per key = f / α ≈ (log2(1/ε) + 3) / 0.955     for b = 4
```

For `ε = 1%` that is `f = 10` and about 10.5 bits per key, against 9.6 for a Bloom filter. Without
further tricks, a cuckoo filter is only smaller than a Bloom filter below a false-positive rate of
roughly 0.4%. The 3% crossover quoted above comes from the paper's semi-sorting variant.

Size the table as `m = next_power_of_two(ceil(n / (b · α)))` for an expected `n` keys. A cuckoo
filter cannot grow in place, because the original keys are needed to rehash into a larger table.

### Variants

- **Semi-sorted buckets:** with `b = 4`, the order of fingerprints in a bucket does not matter.
  Sorting them and storing an index into the table of all possible sorted bucket contents saves
  one bit per fingerprint, which brings the cost to `(log2(1/ε) + 2) / α`. The price is an
  encode and decode on every access.
- **Bucket shape:** the paper finds `b = 2` slightly smaller for `ε > 0.2%`, and `b = 4` best from
  there down to `ε = 0.001%`. Larger buckets raise `α` but cost more fingerprint bits.

## Invariants

- Every non-empty slot holds a non-zero fingerprint.
- For every stored fingerprint `fp` in bucket `i`, its alternate bucket is `i xor hash(fp)`, and
  the alternate of the alternate is `i` again.
- After a successful insert, a lookup of that key answers "maybe present" until the key is
  deleted.
- The number of non-empty slots (plus a victim, if one is held) equals inserts minus successful
  deletes.
- No fingerprint appears more than `2b` times across a key's two buckets.

## Implementation pitfalls

- **The homeless fingerprint on failure.** When the kick loop gives up, the fingerprint in hand
  was evicted from the table, so returning "full" silently creates a false negative for some
  earlier key. The authors' reference implementation keeps a one-entry victim slot that lookups
  and deletes also check, and refuses further inserts while it is occupied. Recording the kick
  path and undoing it is the other option.
- **Infinite or unbounded kicking.** Without the `MAX_KICKS` bound, a full table loops forever.
  Kicks should pick a random slot; always evicting slot 0 can cycle between two buckets.
- **Duplicate inserts.** Inserting the same key `2b + 1` times fails, because all copies compete
  for the same `2b` slots. If the filter does not need deletes, skip the insert when the key is
  already present. If it does, keep duplicates bounded or count them outside the filter.
- **Correlated index and fingerprint.** If `i1` and `fp` come from the same bits of the hash, keys
  in the same bucket share fingerprints and the false-positive rate rises. Take them from
  different halves of a 64-bit hash.
- **Short fingerprints in large tables.** With `f`-bit fingerprints a key can only reach `2^f`
  alternate buckets from `i1`. When `2^f` is much smaller than `m`, inserts start failing at a
  lower load factor than the formula above assumes.
- **Unstable hashing.** As with Bloom filters, Rust's `DefaultHasher` and `RandomState` change
  between processes, so a filter written with them cannot be read back. Use a fixed, specified
  hash and store the seed with the filter.
- **Index arithmetic.** In this crate clippy denies plain slice indexing and arithmetic that can
  overflow. Mask the index with `m - 1` (or reduce it with a widening multiply-shift), read slots
  with `get`, and compute the bucket offset `i · b + slot` with checked operations.

## Testing

- No false negatives: insert a random set until the table reports full, then assert every
  successfully inserted key answers "maybe present". Repeat with many seeds.
- False-positive rate: query 100,000 keys that were never inserted and check the measured rate
  is below the `2b / 2^f` bound.
- Load factor: insert until the first failure with `b = 4` and a fingerprint of at least 8 bits,
  and check that the table reached roughly 95% occupancy.
- Delete: insert a set, delete half, and check the deleted keys mostly answer "absent" while
  every remaining key still answers "maybe present". Include two keys forced into the same
  bucket with the same fingerprint, and delete one of them.
- Alternate index: a property test that `alt(alt(i, fp), fp) == i` for random `i` and `fp`.
- Edge cases: an empty filter, a filter of one bucket, the fingerprint that hashes to 0, inserting
  the same key `2b + 1` times, and deleting a key that is absent.

## Further reading

- [Cuckoo Filter: Practically Better Than
  Bloom](https://www.cs.cmu.edu/~dga/papers/cuckoo-conext2014.pdf), Fan, Andersen, Kaminsky and
  Mitzenmacher, 2014.

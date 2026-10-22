# Linear hashing

> Builds on [slotted page](../storage-primitives/slotted-page.md) and [buffer
> pool](../storage-primitives/buffer-pool.md).

An on-disk hash index that grows one bucket at a time in a fixed round-robin order, with no
directory, using overflow pages in the meantime.

- **Used by:** Berkeley DB's hash access method; PostgreSQL hash indexes grow in a similar
  bucket-at-a-time way.
- **Cost:** about one page read per lookup, more when overflow chains grow.
- **Notes:** Litwin, 1980.

## How it works

### State

The file starts with `N` buckets, numbered `0` to `N - 1`. Each bucket is a primary
[slotted page](../storage-primitives/slotted-page.md), plus a chain of overflow pages when it holds
more than one page's worth of entries. Beyond the buckets themselves, the whole index is described
by two numbers kept in a metadata page:

- the **level** `ℓ`, the number of times the bucket count has doubled;
- the **split pointer** `s`, the next bucket to split, with `0 ≤ s < N · 2^ℓ`.

The file then has `N · 2^ℓ + s` buckets. Two hash functions are in use at any moment, both derived
from one hash `h`:

```text
h_ℓ(k)     = h(k) mod (N · 2^ℓ)
h_(ℓ+1)(k) = h(k) mod (N · 2^(ℓ+1))
```

Buckets before `s` have already been split in this round, so their keys are spread with the finer
function `h_(ℓ+1)`, and so are the new buckets created by those splits. The others still use
`h_ℓ`. In the figure below `ℓ = 0`, so `h₀` is `h_ℓ` and `h₁` is `h_(ℓ+1)`.

<figure class="diagram">
{{#include figures/linear-hashing-state.svg}}
<figcaption>N = 4, ℓ = 0 and s = 2, with keys shown by their hash. Buckets 0 and 1 have been split into 4 and 5, and these four use <code>h₁</code> (mod 8); buckets 2 and 3 still use <code>h₀</code> (mod 4). A key with hash 13 has <code>h₀ = 1</code>, which is below <code>s</code>, so it lives in bucket 13 mod 8 = 5.</figcaption>
</figure>

There is no directory. Because buckets are created in order, a bucket number can be computed from
the hash alone.

### Address of a key

1. Compute `b = h_ℓ(k)`.
2. If `b < s`, bucket `b` has already been split this round, so use `b = h_(ℓ+1)(k)` instead. The
   result is either `b` or `b + N · 2^ℓ`.

When `N` is a power of two, both `mod` operations are bit masks. PostgreSQL's hash index computes
its bucket the same way with two masks: `b = h & highmask`, and if `b` is beyond the last bucket,
`b = b & lowmask`.

### Lookup

1. Compute the key's bucket.
2. Search the bucket's primary page, then each page of its overflow chain, until the key is found
   or the chain ends.

### Insert

1. Compute the key's bucket and add the entry to its primary page, or to an overflow page if the
   primary page is full.
2. Check the split trigger. The two common choices are to split whenever an insert overflows a
   page (uncontrolled splitting), or only when the load factor, `entries / (buckets · entries per
   page)`, exceeds a threshold (controlled splitting).
3. If it fires, split bucket `s`, which is usually not the bucket that overflowed:
   1. Append a new bucket, number `s + N · 2^ℓ`.
   2. Rehash every entry of bucket `s`, including its overflow pages, with `h_(ℓ+1)`. Each one stays
      in `s` or moves to the new bucket.
   3. Free overflow pages that are now empty.
   4. Advance `s` by one. If `s` reaches `N · 2^ℓ`, the round is complete: increase `ℓ` by one and
      reset `s` to 0.

<figure class="diagram">
{{#include figures/linear-hashing-split.svg}}
<figcaption>Splitting bucket 2 of the figure above. Rehashing its four entries with <code>h₁</code> keeps 2 and 10 in place and moves 6 and 14 to the new bucket 6, that is 2 + 4. The overflow page is now empty and freed, and <code>s</code> moves on to 3.</figcaption>
</figure>

The bucket that overflowed keeps its overflow page until the split pointer reaches it. Over one
round every bucket is split once, so the file grows smoothly by one bucket at a time instead of
doubling at once.

### Delete

1. Remove the entry from its bucket, and free an overflow page if it becomes empty.
2. Optionally contract, the reverse of a split, when the load factor drops below a lower threshold:
   1. If `s = 0`, decrease `ℓ` by one and set `s = N · 2^ℓ`.
   2. Decrease `s` by one.
   3. Move every entry of the last bucket, `s + N · 2^ℓ`, back into bucket `s`, and remove the last
      bucket.

### Variants

- **Split trigger:** splitting on every overflow grows the file sooner and keeps chains short;
  splitting on a load-factor threshold packs pages more fully, at the price of longer chains in
  between.
- **In-memory linear hashing:** Larson's dynamic hash tables use the same split pointer for a hash
  table in memory, growing a bucket array one bucket at a time so that no single insert pays for
  rehashing the whole table.
- **Mapping bucket numbers to pages:** bucket `b` cannot live at page `b` if overflow pages
  are allocated in the same file. PostgreSQL allocates bucket pages in groups and keeps a small
  array in its metadata page from which it computes the block of any bucket number.

## Invariants

- The number of buckets is `N · 2^ℓ + s`, with `0 ≤ s < N · 2^ℓ`.
- Every key is stored in the bucket its address function gives, using the current `ℓ` and `s`.
- For buckets `b < s` and `b ≥ N · 2^ℓ`, every key satisfies `h_(ℓ+1)(k) = b`. For buckets
  `s ≤ b < N · 2^ℓ`, every key satisfies `h_ℓ(k) = b`.
- With controlled splitting, the load factor is at most the threshold after every insert.
- Every inserted, undeleted key is found by lookup, and no key is stored twice.

## Implementation pitfalls

- **`≤` instead of `<`.** Bucket `s` has not been split yet, so a key with `h_ℓ(k) = s` belongs in
  bucket `s`. Using `b ≤ s` sends it to a bucket that does not exist yet, or to the wrong one.
- **Splitting the bucket that overflowed.** It is tempting, but buckets must split in order for the
  address function to work. The overflowing bucket waits for the split pointer, and its overflow
  chain holds the extra entries until then.
- **Forgetting the overflow chain during a split.** Entries in overflow pages need rehashing too;
  leaving them behind makes them unreachable once their key's address changes.
- **Updating the metadata too early.** If `s` is advanced before the moved entries are written, a
  crash leaves keys whose address now points to a bucket that does not hold them. Write the new
  bucket, then the shrunken old bucket, and update `ℓ` and `s` last, or log the whole split in the
  [write-ahead log](../storage-primitives/write-ahead-log.md).
- **Unstable hashing.** Every key's bucket is computed from its hash, so the hash must be the same
  in every process and Rust version. `DefaultHasher` is not; use a hash with a fixed specification
  and seed.
- **Long chains under skew.** The split order ignores where the keys are. A hot bucket immediately
  behind the split pointer waits almost a full round for its split, and its chain grows meanwhile.
  Measure the longest chain in tests, not only the average.
- **Overflowing arithmetic.** `N · 2^ℓ` overflows for large `ℓ`. In this crate clippy denies
  unchecked arithmetic and shifts, so use `checked_shl` and `checked_mul`, and turn an overflow
  into an error.

## Testing

- Reference model: apply random inserts, updates and deletes to the index and to a `HashMap`, and
  compare every lookup. After every operation, check the invariants, including that every key sits
  in the bucket its address function gives.
- Address function: a table-driven test of `(hash, N, ℓ, s) → bucket`, covering `b < s`, `b = s`
  and `b > s`, and the wrap from `s = N · 2^ℓ - 1` to the next level.
- Split order: with the identity as the hash function on integer keys and tiny pages, insert keys
  one by one and assert that buckets split in the order 0, 1, 2, …, and that `ℓ` increases after
  each round.
- Load factor: with controlled splitting, assert the load factor never exceeds the threshold after
  an insert.
- Skew: insert keys that all hash to one bucket and check that lookups stay correct while its chain
  grows, and that the chain is redistributed when the split pointer reaches it.
- Contraction: grow the index, delete every key, and check that it shrinks back to `N` buckets with
  `ℓ = 0` and `s = 0`, if contraction is implemented.
- Persistence: run through the buffer pool, flush, reopen, and compare with the model.

## Further reading

- Litwin, *Linear Hashing: A New Tool for File and Table Addressing*, VLDB 1980.
- [Dynamic Hash Tables](https://doi.org/10.1145/42404.42410), Larson, 1988. Linear hashing for
  in-memory tables, with a clear account of the split pointer.

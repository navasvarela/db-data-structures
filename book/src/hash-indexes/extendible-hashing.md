# Extendible hashing

> Builds on [slotted page](../storage-primitives/slotted-page.md) and [buffer
> pool](../storage-primitives/buffer-pool.md).

An on-disk hash index whose directory of bucket pointers doubles when a bucket overflows. Only the
overflowing bucket is split, so growth never rehashes the whole table.

- **Cost:** one or two page reads per lookup. The directory doubles in size under skewed keys.
- **Notes:** Fagin et al., 1979.

## How it works

### Layout

There are two kinds of page:

- **Buckets**, each a [slotted page](../storage-primitives/slotted-page.md) holding key-value
  entries, and each with a **local depth** `L`: all keys in the bucket agree on the last `L` bits of
  their hash.
- A **directory**, an array of `2^G` bucket page IDs, where `G` is the **global depth**. Entry `i`
  points to the bucket for keys whose hash ends in the `G` bits of `i`.

A bucket with local depth `L < G` is shared: `2^(G - L)` directory entries point to it, all those
whose last `L` bits match the bucket's.

<figure class="diagram">
{{#include figures/extendible-hashing-layout.svg}}
<figcaption>A directory of global depth 2 over three buckets. In this figure and the ones below, a bucket holds at most two entries, and <code>…100</code> stands for a hash ending in the bits 100.</figcaption>
</figure>

Entries `00` and `10` both point to bucket A, because it has only split on one bit. Buckets B and C
have each split on two bits and own one entry each.

This page uses the last (least significant) bits of the hash. Using the first bits works equally
well, with the differences noted under variants.

### Lookup

1. Compute the key's hash `h`.
2. Take its last `G` bits: `i = h & (2^G - 1)`.
3. Read directory entry `i` to get the bucket's page ID.
4. Fetch the bucket and search it for the key.

If the directory is held in memory, a lookup reads one page. If the directory itself has to be read
from disk, it reads two, which is the bound the paper states.

### Insert

1. Find the bucket as in lookup. If the key is there, update its value. If the bucket has room,
   add the entry and stop.
2. Otherwise the bucket of local depth `L` must split. If `L = G`, no spare directory entry can
   point to a new bucket, so double the directory first:
   1. For every entry `i` in `0..2^G`, set entry `i + 2^G` to the same bucket as entry `i`.
   2. Increase `G` by one.
3. Split the bucket:
   1. Allocate a new bucket. Give both buckets local depth `L + 1`.
   2. Move every entry whose hash has bit `L` set (counting from 0) to the new bucket; the rest
      stay.
   3. Repoint the directory: every entry that pointed to the old bucket and has bit `L` of its
      index set now points to the new bucket. There are `2^(G - L - 1)` of them.
4. Retry the insert from step 1. If every key in the bucket shared bit `L`, one bucket is still
   full, and the loop splits again.

Only one bucket's entries move per split; nothing else is rehashed.

<figure class="diagram">
{{#include figures/extendible-hashing-split.svg}}
<figcaption>Inserting k6 (…110) into the full bucket A of the first figure. Its local depth 1 is below the global depth, so no doubling is needed: A splits on bit 1, k4 and k6 move to the new bucket D, and only entry 10 is repointed.</figcaption>
</figure>

<figure class="diagram">
{{#include figures/extendible-hashing-double.svg}}
<figcaption>Inserting k7 (…1001) into the full bucket B of the first figure instead. B's local depth equals the global depth, so the directory first doubles: entries 100 to 111 copy entries 000 to 011. Then B splits on bit 2, k5 moves to the new bucket E, and entry 101 is repointed to it.</figcaption>
</figure>

### Delete

1. Find the bucket and remove the entry.
2. Optionally merge. A bucket of local depth `L` has a *buddy*, the bucket whose bits match on the
   last `L - 1` bits but differ in bit `L - 1`. If the buddy has the same local depth and the two
   fit in one page, move the entries into one bucket, free the other, decrease `L`, and point all
   of their directory entries at the survivor.
3. Optionally shrink. If every bucket now has `L < G`, each entry `i + 2^(G - 1)` points to the same
   bucket as entry `i`, so the upper half of the directory can be dropped and `G` decreased.

Many implementations skip merging, as PostgreSQL's hash index does for its own scheme, because
deletes rarely free enough space to be worth the complexity.

### Variants

- **First bits instead of last bits:** the index is the first `G` bits of the hash. Doubling then
  places each entry's copy right next to it (entry `i` becomes `2i` and `2i + 1`), instead of
  appending a copy of the whole array. Splitting and lookup are otherwise the same.
- **Overflow pages:** when more keys than fit in a bucket share the same hash, no split can separate
  them. Allowing a chain of overflow pages on a bucket handles this case, as
  [linear hashing](linear-hashing.md) does routinely.
- **Directory across pages:** a directory larger than one page is itself paged through the
  [buffer pool](../storage-primitives/buffer-pool.md). Doubling then writes every directory page.

## Invariants

- The directory has exactly `2^G` entries.
- Every bucket has `L ≤ G`, and exactly `2^(G - L)` directory entries point to it, namely those
  whose last `L` bits equal the bucket's bit pattern.
- Every key in a bucket has a hash whose last `L` bits equal that pattern.
- No bucket holds more entries than fit in a page, unless overflow pages are allowed.
- If the directory is shrunk whenever possible, `G` equals the largest local depth.
- Every inserted, undeleted key is found by lookup, and no key is found in two buckets.

## Implementation pitfalls

- **Unstable hashing.** The directory index of every key on disk depends on the hash. Rust's
  `DefaultHasher` and `RandomState` are seeded per process, so an index written with them is
  unreadable after a restart. Use a hash with a fixed specification and seed, and record which one
  in the index's metadata page.
- **Assuming one split is enough.** If all entries go to one side of the split, the insert still
  does not fit. Loop until it does, which may double the directory more than once.
- **Splitting forever.** More identical hashes than fit in a bucket can never be separated, and a
  naive loop doubles the directory until memory runs out. Cap the global depth, for example at the
  number of hash bits, and fall back to overflow pages or an error.
- **Repointing one directory entry instead of all.** After a split, every entry with the right bit
  pattern must move, not only the one the insert came through. Iterate over all indexes `i` with
  `i & (2^L - 1)` equal to the old bucket's pattern.
- **Mixing first-bit and last-bit code.** Doubling, splitting and lookup must agree on which bits
  they use. A mix works until the first doubling, then silently loses keys.
- **Shifts that overflow.** `1 << G` panics in debug builds once `G` reaches the integer's width,
  and clippy denies unchecked shifts in this crate. Use `checked_shl`, and store `G` and `L` in a
  small type such as `u8`.
- **Crash during a split.** A split writes the new bucket, the old bucket and the directory. A crash
  in between can leave a key in neither bucket, or in both. Log the split in the
  [write-ahead log](../storage-primitives/write-ahead-log.md), or order the writes so that every
  intermediate state on disk still finds every key: new bucket first, then directory, then the
  removal from the old bucket. The old bucket may then still hold stale copies of moved entries
  after a crash, which recovery has to remove.
- **Directory size under skew.** One bucket that keeps splitting doubles the whole directory each
  time, even though every other bucket is shared by many entries. Monitor `G` against the number of
  buckets in tests with skewed keys.

## Testing

- Reference model: apply random inserts, updates and deletes to the index and to a `HashMap`, and
  compare every lookup. Check the invariants above after every operation.
- Tiny buckets: use buckets that hold two or three entries, so a few hundred inserts cause many
  splits and doublings.
- Controlled hashes: make the hash function a parameter, and use the identity on integer keys in
  tests. That lets a test build exact cases: a split without doubling, a split with doubling, and a
  split that has to repeat because every key shares the next bit.
- Identical hashes: insert more keys with the same hash than a bucket holds, and assert the
  documented behaviour (overflow page or error) instead of a hang.
- Merging and shrinking: insert many keys, delete them all, and check that the directory and bucket
  count shrink back if merging is implemented.
- Persistence: run through the buffer pool, flush, reopen from the files, and compare with the
  model.

## Further reading

- [Extendible Hashing — A Fast Access Method for Dynamic
  Files](https://doi.org/10.1145/320083.320092), Fagin, Nievergelt, Pippenger and Strong, 1979.
- [CMU 15-445](https://15445.courses.cs.cmu.edu/fall2025/), the lecture on hash tables.

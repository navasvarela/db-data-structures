# Hash table

> In this crate: `std::collections::HashMap` inside `hash::simple::SimpleHashIndex`, in
> [`src/hash/simple.rs`](https://github.com/navasvarela/db-data-structures/blob/main/src/hash/simple.rs).

Maps keys to values through a hash function. It answers point lookups in constant time, but loses
key order, so it cannot serve range queries.

- **Used by:** the Bitcask keydir, hash joins and hash aggregation in query engines, buffer pool
  page tables, lock tables.
- **Cost:** expected `O(1)` lookup and insert; resizing is `O(n)` but amortised.

## How it works

### Layout

A hash table is an array of `capacity` slots and a hash function that turns a key into a 64-bit
number. The hash, reduced to the range `0..capacity`, gives the key's *home slot*. Two keys with
the same home slot *collide*, and the two families of hash tables differ in how they resolve that:

- **Separate chaining:** each slot holds a list (or other small container) of every entry whose
  home it is. A lookup hashes to the slot and searches its list.
- **Open addressing:** each slot holds at most one entry. A key that finds its home occupied
  follows a fixed *probe sequence* of other slots until it finds a free one. Lookups follow the
  same sequence.

This page concentrates on open addressing, which is what Rust's `HashMap` uses. With *linear
probing*, the simplest probe sequence, the next slot is the one to the right, wrapping around:

<figure class="diagram">
{{#include figures/hash-table-layout.svg}}
<figcaption>Four keys in a table of capacity 8, with <code>home(k) = hash(k) &amp; 7</code>. With chaining, "cat" and "dog" share slot 1's list. With linear probing, "dog" also hashes to 1 and probes on to slot 2; "eel" hashes to 2, finds it taken by "dog", and probes on to slot 3.</figcaption>
</figure>

Each slot is in one of three states: *empty* (never used, or safely cleared), *full* (holds an
entry), or *deleted*, a marker called a *tombstone* that is explained under Delete. The table
also tracks `len`, the number of full slots, and the *load factor*, `len / capacity`. Keeping
`capacity` a power of two lets `hash & (capacity - 1)` replace a division.

### Lookup

1. Hash the key and compute its home slot.
2. Look at the slot:
   - *empty*: the key is not in the table, because an insert of it would have stopped here.
   - *full* with an equal key: found.
   - *full* with another key, or *deleted*: move to the next slot in the probe sequence and repeat.

Comparing the full 64-bit hash, stored next to each entry, before comparing keys saves an
expensive key comparison on most mismatches.

### Insert

1. If inserting would push the load factor (counting tombstones) over the maximum, resize first.
2. Probe from the home slot as in a lookup. If an equal key is found, replace its value.
3. Otherwise, store the entry in the first *deleted* or *empty* slot the probe passed, and
   increase `len`. Reusing the first tombstone keeps probe sequences short.

Probing has to continue past tombstones to the first empty slot before deciding the key is new,
or a key stored beyond a tombstone would be inserted a second time.

### Delete

Marking a slot *empty* breaks lookups. In the diagram, emptying slot 2 would make a lookup for
"eel" stop at slot 2 and report it absent. There are two correct ways to delete:

- **Tombstones:** mark the slot *deleted*. Lookups pass over it, inserts may reuse it. Tombstones
  still lengthen probes, so they count towards the load factor, and a resize (or an in-place
  rehash) removes them.
- **Backward-shift deletion** (linear probing only): empty the slot, then walk right over the
  following full slots. Move each entry back into the hole if the hole lies between its home
  slot and its current slot (cyclically), and make its old slot the new hole. Stop at the first
  empty slot. This leaves no tombstones at all; it is Knuth's Algorithm R.

<figure class="diagram">
{{#include figures/hash-table-delete.svg}}
<figcaption>Deleting "dog" from slot 2 of the table above. Emptying the slot makes "eel" unreachable from its home slot 2. A tombstone keeps the probe going. Backward shifting moves "eel" into the hole, because its home, slot 2, is the hole itself; the walk then stops at the empty slot 4.</figcaption>
</figure>

### Resize

1. Allocate a new array, usually twice the capacity.
2. Re-insert every full entry from the old array, recomputing its home slot from its hash. Skip
   empty slots and tombstones.
3. Replace the old array.

Each resize costs `O(n)`, but doubling means each entry is moved a constant number of times on
average, so inserts stay amortised `O(1)`.

### Variants

- **Robin Hood hashing:** linear probing where an insert that meets a resident closer to its own
  home than the new entry is to its home swaps the two, and continues inserting the evicted one.
  Probe lengths become much more even, and a lookup can stop early once it passes a resident
  whose distance from home is smaller than the distance it has travelled.
- **Swiss tables:** the layout of Abseil's `flat_hash_map` and of `hashbrown`, which is Rust's
  `HashMap`. The hash yields `H1`, which picks where probing starts, and a 7-bit `H2`; Abseil
  splits the hash into 57 and 7 bits, while `hashbrown` takes `H2` from the top 7 bits and starts
  probing from the whole hash. A separate array of one-byte *control bytes* holds, for every slot, either `H2` of its
  entry or a special value for empty or deleted. A lookup loads a *group* of control bytes at once
  (16 with SSE2, fewer on other targets), compares all of them with `H2` in a single SIMD
  operation, and compares keys only for the matching slots. If the group holds an empty byte the
  key is absent; otherwise probing jumps to the next group, by a stride that grows by one group
  each time. `hashbrown` keeps the load factor at or below 7/8 for tables of more than eight
  slots, and on deletion writes *empty* instead of *deleted* when it can prove no probe sequence
  ever continued past that group.
- **Cuckoo hashing:** each key has two possible slots, from two hash functions. An insert that
  finds both taken evicts one resident to its other slot, possibly cascading. Lookups check
  exactly two slots, which makes them fast in the worst case; inserts can fail and force a
  rehash. The [cuckoo filter](../filters/cuckoo-filter.md) uses the same idea.
- **Growing one bucket at a time:** [linear hashing](../hash-indexes/linear-hashing.md) and
  [extendible hashing](../hash-indexes/extendible-hashing.md) avoid the stop-the-world resize,
  which matters when the table lives on disk.

In this crate, `SimpleHashIndex` keeps its keydir in a `HashMap<String, RecordLocation>`, so it
gets a Swiss table with a randomly seeded SipHash-1-3, which is the standard library's default.
A hand-written table would make a good replacement exercise, with the existing tests as its
safety net.

## Invariants

- Every stored key can be reached from its home slot by following the probe sequence without
  crossing an *empty* slot.
- Each key is stored at most once.
- `len` equals the number of full slots, and `len + tombstones` stays at or below the maximum load
  times `capacity` after every operation.
- At least one slot is *empty*, so a lookup for an absent key always terminates.
- `capacity` is a power of two, if home slots are computed with a mask.
- For Robin Hood hashing: walking along a run of full slots, no entry is further than one step
  more from its home than the entry before it is from its own home.

## Implementation pitfalls

- **Deleting by emptying the slot.** It silently hides every key that probed past it. The bug
  only shows up after a collision, so tests with few keys and a good hash never catch it.
- **Tombstone build-up.** A workload that inserts and deletes different keys fills the table with
  tombstones while `len` stays small. If they do not count towards the load factor, every
  unsuccessful lookup ends up scanning the whole table, or never ends once no empty slot is left.
- **Weak hashes with a power-of-two mask.** `hash & mask` keeps only the low bits. An identity
  hash of integers that are multiples of 8, for example, puts every key in one slot out of eight.
  Use a hash that mixes all input bits into the low ones.
- **Unstable hashing.** `RandomState` is seeded per process, so home slots differ after a restart.
  That is fine for an in-memory table rebuilt on open, like this crate's keydir, but anything
  persisted by hash (a hash index on disk, spilled partitions) needs a hash with a fixed
  specification and seed.
- **Untrusted keys.** A fixed, fast hash lets an attacker choose keys that all collide and turn
  every operation into a linear scan. That is why the standard library defaults to a randomly
  seeded SipHash.
- **Hash and equality disagreeing.** Keys that compare equal must hash equally. With `Borrow`
  lookups (a `HashMap<String, _>` queried with a `&str`), the borrowed form must hash exactly
  like the owned one. A key mutated while stored, for example through a `Cell`, is lost.
- **Slot state as `Option`.** Three states do not fit in `Option<(K, V)>`; use an enum such as
  `Empty`, `Deleted` and `Full(K, V)`, so that a match forces every probe step to handle each one.
- **Arithmetic in this crate.** Clippy denies indexing and overflowing arithmetic. Advance probes
  with `wrapping_add` followed by `& mask`, read slots with `get`, and compute capacities and
  load limits with `checked_mul`, returning an error rather than wrapping when they overflow.
- **Iteration order.** It is arbitrary and changes on resize. `SimpleHashIndex::compact` copies
  records in keydir order, which is safe because each key appears once, but tests must not
  depend on that order.

## Testing

- Reference model: run random sequences of insert, get and remove against the table and a
  `std::collections::HashMap` (or `BTreeMap`, whose order makes failures easier to read), with a
  small key space so that overwrites and removals of existing keys are common.
- Collision stress: plug in a hasher that returns a constant, or only a few distinct values, so
  that every operation exercises probing, tombstones and backward shifting.
- Churn: repeatedly insert and remove distinct keys, far more times than the capacity, and assert
  that capacity stays bounded and lookups of absent keys still terminate.
- Structure checks after each operation: `len` matches the full slots, every key is reachable from
  its home slot, and the load limit holds.
- Edge cases: an empty table, removal from an empty table, inserts that cross the resize
  threshold exactly, and an empty key.

## Further reading

- [A Seven-Dimensional Analysis of Hashing Methods and its Implications on Query
  Processing](https://www.vldb.org/pvldb/vol9/p96-richter.pdf), Richter, Alvarez and Dittrich, 2015.
  Chaining, linear probing, Robin Hood and cuckoo hashing compared on database workloads.
- [Swiss Tables design notes](https://abseil.io/about/design/swisstables). The SIMD-probed
  open-addressing layout behind Rust's standard `HashMap`, through `hashbrown`.
- [Dynamic Hash Tables](https://doi.org/10.1145/42404.42410), Larson, 1988. Growing a hash table one
  bucket at a time instead of rehashing everything.

# Finite state transducer

A minimal automaton built from sorted keys that maps each key to an output value, sharing both
common prefixes and common suffixes. It makes a compact term dictionary that still answers exact,
prefix, range and fuzzy (Levenshtein automaton) lookups.

- **Used by:** Lucene's terms index (over its block-based term dictionary), Tantivy (through
  `tantivy-fst`, a fork of the `fst` crate).
- **Cost:** often smaller than the raw key bytes; lookups cost `O(k)` for a `k`-byte key.
- **Notes:** Mihov and Maurel, 2001. Building it needs keys in sorted order, as an SSTable does.

## How it works

### Layout

A finite state transducer (FST) here is a deterministic, acyclic automaton over bytes:

- **States**, one of them the start state. Some states are *final*: a key that ends in a final
  state is in the map.
- **Transitions**, each labelled with one input byte and an *output*, here a `u64`. A state has at
  most one transition per byte, kept in byte order.
- A **final output** on each final state, added when a key ends there.

A key's value is the sum of the outputs along its path, plus the final output of the state it ends
in. Without outputs the same structure is a finite state acceptor, which stores a set.

The map `{cat: 4, cats: 9, dog: 7, dogs: 12}`, with a double circle marking a final state and
`b/o` meaning "byte `b`, output `o`":

<figure class="diagram">
{{#include figures/finite-state-transducer-map.svg}}
<figcaption>The minimal FST for the four keys. Every final output is 0 here. Looking up <code>cats</code> follows the highlighted path and sums its outputs.</figcaption>
</figure>

A trie would need separate states for the endings of `cat`, `cats`, `dog` and `dogs`. Here the
state after `cat` and the state after `dog` are the same state, because everything that can follow
them, and the outputs collected on the way, are identical. Sharing suffixes as well as prefixes is
what makes an FST much smaller than a trie.

<figure class="diagram">
{{#include figures/finite-state-transducer-trie.svg}}
<figcaption>The same map as a trie, with outputs already pushed towards the start. It has nine states; the two highlighted pairs have the same final flag, final output and outgoing transitions, so the FST keeps one state of each pair and has seven.</figcaption>
</figure>

Outputs are pushed as close to the start state as possible: `c` carries all of `cat`'s value, so
the transitions after it carry 0, and the shared tail can serve both words.

### Serialised form

The built FST is a single byte array. Each state is written once all the states it points to have
been written, so the start state comes last, and transitions refer to their targets by byte offset.
The `fst` crate packs states so tightly that, according to its author, most of them take a single
byte. Reading the FST then means following offsets in a byte slice, which can be a memory-mapped
file.

### Lookup

1. Start at the start state, with a running sum of 0.
2. For each byte of the key, find the transition with that label (binary search, or a direct table
   for dense states). If there is none, the key is absent. Otherwise add its output to the sum and
   move to its target.
3. When the key is consumed, the key is present only if the state is final. The value is the sum
   plus the state's final output.

This costs `O(k)` transitions for a `k`-byte key, independent of the number of keys.

### Build

The construction of Daciuk et al. (for sets) and Mihov and Maurel (for maps) builds the minimal FST
in one pass over keys in sorted order. It keeps the path of the previous key as a stack of
*unfinished* states, which may still gain transitions, and a *registry*: a hash map from the
content of each finished state to its offset in the output.

For each new key, which must be strictly greater than the previous one:

1. Find the length `p` of the common prefix of the new key and the previous key.
2. **Freeze the old suffix.** Pop unfinished states deeper than `p`, deepest first. For each, look
   it up in the registry by its content: final flag, final output, and every transition's byte,
   output and target offset. If an equal state exists, reuse its offset; otherwise write the state
   and register it. Point the parent's last transition at the resulting offset.
3. **Add the new suffix.** Push one unfinished state per remaining byte of the new key, and mark the
   last one final.
4. **Share the output.** Walk the `p` transitions of the common prefix with the new key's value
   `v` remaining. At each transition with output `o`, keep `min(o, v)` on the transition, push the
   excess `o - min(o, v)` onto every transition of the next state (and its final output, if it is
   final), and subtract `min(o, v)` from `v`. Put what is left of `v` on the first new transition.
   Because the new key is strictly greater than the previous one, it always has at least one byte
   past the common prefix, so that transition exists.
5. When there are no more keys, freeze every remaining state, including the start state, and record
   the start state's offset.

States deeper than `p` can be frozen because every later key is greater, so none of them can ever
gain a transition again. Each frozen state's children are already frozen, so comparing children by
offset is enough to decide equivalence.

### Range, prefix and fuzzy queries

Because transitions are kept in byte order, a depth-first walk that always takes the smallest byte
first enumerates keys in sorted order. Range and prefix queries prune the walk:

- **Prefix:** follow the prefix as in a lookup, then enumerate everything below.
- **Range:** walk as above, but skip transitions that lead entirely below the lower bound, and stop
  at the first key above the upper bound.
- **Fuzzy, regular expression and other automata:** run a second automaton alongside the walk,
  for example a Levenshtein automaton that accepts every string within edit distance `n` of the
  query. Feed it each byte taken; if it reports that no continuation can ever match, skip that
  whole subtree. The result is the intersection of the two automata, and it visits only a small
  part of the FST.

### Variants

- **Approximately minimal FSTs.** A registry that remembers every frozen state needs memory
  proportional to the FST. The `fst` crate bounds it, as a fixed-size hash table with a small
  cache per slot, so frequently shared states are found and rare ones may be written twice. The
  result is close to minimal, built in constant memory, and can be streamed straight to a file.
- **Other output types.** Any outputs with addition, subtraction and a "common prefix" operation
  work. For integers the common prefix is `min`; for byte strings it is the longest common prefix.
  Lucene's terms index uses an FST with byte-string outputs: it maps term prefixes to the on-disk
  blocks of the term dictionary that hold the terms with that prefix.

## Invariants

- Keys were added in strictly increasing byte order, with no duplicates.
- The automaton is deterministic (at most one transition per byte per state) and acyclic, and every
  state's transitions are sorted by byte.
- The set of keys accepted is exactly the set of keys added, and each key's output sum equals its
  value.
- Enumerating from the start state yields the keys in increasing byte order.
- Every transition's target offset points to a state written earlier in the byte array.
- With an unbounded registry, no two written states are equivalent, which makes the FST minimal.

## Implementation pitfalls

- **Out-of-order and duplicate keys.** They must be rejected with an error, not silently accepted,
  since either breaks the freezing argument. The `fst` crate has separate `OutOfOrder` and
  `DuplicateKey` errors.
- **The empty key.** It makes the start state final, with a final output. Test it.
- **Comparing states before their children are final.** Two states are only equivalent if their
  targets are the same *offsets*, which only exist once the children are frozen. Comparing
  unfinished states merges suffixes that later diverge, and the FST then accepts keys that were
  never added.
- **Forgetting outputs in equivalence.** Two states with the same transitions but different outputs
  or final outputs are different states. Hash and compare all of them.
- **Output arithmetic.** In step 4 the subtraction `o - min(o, v)` cannot underflow, but this crate
  denies unchecked arithmetic, so use `checked_sub` and turn a `None` into an internal error rather
  than a panic (`unreachable!` is denied too).
- **Untrusted bytes.** A corrupt FST can contain offsets past the end or pointing forwards, which
  could loop forever. Bounds-check every offset with `get`, and check that each target offset is
  smaller than the current state's.
- **Recursion during enumeration.** Keys can be long, so walk with an explicit stack of
  `(state, next transition)` rather than recursion.
- **Byte order versus string order.** The FST orders keys by bytes. Rust's `str` and `[u8]`
  comparisons agree with this for UTF-8, but a locale-aware collation does not. A Levenshtein
  automaton over bytes also counts edits in bytes, not characters, unless it is built over UTF-8
  sequences.

## Testing

- **Model test.** Build an FST from a random sorted `BTreeMap<Vec<u8>, u64>`, and check every key's
  lookup, a sample of absent keys, full enumeration, and random prefix and range queries against the
  map.
- **Shared structure.** Generate keys with long shared prefixes and suffixes (words with common
  endings, numbers as decimal strings) and keys that are prefixes of other keys.
- **Minimality.** For small inputs, build a trie, minimise it by brute force (merge states with
  equal content bottom-up), and check the FST has the same number of states. Track the serialised
  size on a fixed word list as a regression test.
- **Fuzzy queries.** Compare Levenshtein results with a brute-force edit-distance computation over
  every key, for distances 0, 1 and 2.
- **Errors.** Check that out-of-order and duplicate keys are rejected, and fuzz the reader with
  random and truncated byte arrays; it must return errors, never panic or loop.

## Further reading

- [Index 1,600,000,000 Keys with Automata and Rust](https://burntsushi.net/transducers/), Gallant,
  2015. A long, illustrated walk through building and querying FSTs, by the author of the `fst`
  crate.
- Mihov and Maurel, *Direct Construction of Minimal Acyclic Subsequential Transducers*, 2001.

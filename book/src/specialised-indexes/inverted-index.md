# Inverted index

Maps each term to a sorted list of the documents (and positions) that contain it. Queries intersect
or union posting lists.

- **Used by:** Lucene, Elasticsearch, PostgreSQL GIN indexes for full-text search and arrays.
- **Notes:** posting lists are delta-encoded and compressed; skip pointers speed up intersection.

## How it works

### Layout

An inverted index has two parts:

- a **term dictionary**: every distinct term, in sorted order, with the location of its posting
  list and statistics such as the number of documents that contain it (its *document frequency*).
  It can be a sorted file with a sparse index, a B+ tree, or a
  [finite state transducer](finite-state-transducer.md);
- **posting lists**: for each term, the IDs of the documents that contain it, in increasing order,
  each optionally with the term's frequency in that document and the positions where it occurs.

<figure class="diagram">
{{#include figures/inverted-index-layout.svg}}
<figcaption>Three documents, with word positions counted from 0. Each posting is a document ID with the positions of the term in it; <code>df</code> is the number of postings.</figcaption>
</figure>

Document IDs are small dense integers assigned by the index, not external keys, so that posting
lists compress well. A separate stored-fields file maps a document ID back to the document.

### Compression

Posting lists are stored as gaps rather than IDs: `[3, 7, 8, 20]` becomes `[3, 4, 1, 12]`. Gaps
are small for common terms, so they encode in few bits:

- **Variable-byte:** 7 bits of the number per byte, with the high bit marking "more bytes follow".
  Simple and fast to decode one value at a time.
- **Bit-packing in blocks:** take a fixed-size block of gaps, find the bit width of the largest,
  and pack all of them at that width. Lucene has packed document-ID gaps in blocks of 128 since its
  4.1 postings format, with a variable-byte tail for the last partial block.
- **Patched frame of reference (PFOR):** bit-pack at a width that fits most values, and store the
  few that do not fit as exceptions. One large gap then does not inflate the whole block.

Positions within a document are also gap-encoded, per document.

### Skip data

Intersecting a short list with a long one should not decode the whole long list. Every so many
entries (in Lucene, once per packed block), the writer records a *skip entry*: the last document ID
before a point in the list and the byte offset of that point. An `advance(target)` call uses the
skip entries to jump to the block that may contain `target`, and only decodes that block.

### Build

Documents arrive one at a time, but posting lists are grouped by term, so building is an
external sort. A common approach, called single-pass in-memory indexing in *Introduction to
Information Retrieval*:

1. Assign the next document ID to each incoming document.
2. Split its text into terms with the analyser: tokenise, lower-case, normalise, maybe stem.
3. Append `(doc ID, position)` to the in-memory posting list for each term, in a hash map from term
   to list. Document IDs arrive in order, so every list stays sorted for free.
4. When memory runs out, sort the terms, write the dictionary and the compressed lists as an
   immutable *segment*, and start a new in-memory map.
5. Merge segments in the background: walk the dictionaries of several segments together with a
   [k-way merge](../lsm-trees/k-way-merge-heap.md), concatenate posting lists for equal terms, and
   renumber document IDs of later segments by adding an offset.

### Query

A term query looks the term up in the dictionary and iterates its posting list. Boolean queries
combine iterators that support `next()` and `advance(target)`:

- **AND (intersection):** start with the rarest term. Take its current document `d`, and advance
  every other iterator to `d`. If all land on `d`, it matches; otherwise take the largest document
  they landed on as the new candidate, and repeat.
- **OR (union):** put all iterators in a min-heap keyed by current document ID; pop the smallest,
  collect all iterators on that document, advance them, and push them back.
- **NOT:** iterate the positive side and advance the negative iterator to each candidate; skip the
  candidate if they land on the same document.
- **Phrase:** intersect the terms first, then for each matching document check that the positions
  line up: `"lazy dog"` matches a document where `dog` occurs at `p + 1` for some position `p` of
  `lazy`.

<figure class="diagram">
{{#include figures/inverted-index-intersection.svg}}
<figcaption>An AND of two terms. (1) <code>advance(7)</code> on the long list matches. (2) <code>advance(25)</code> uses the skip entries to jump over the second block and lands on 30. (3) 30 becomes the candidate, and the short list advances to 41. (4) The long list advances to 41: a match.</figcaption>
</figure>

Ranked retrieval scores each matching document, for example with BM25, from term frequencies,
document frequencies and document lengths, and keeps the best `k` in a
[bounded top-k heap](../query-execution/bounded-top-k-heap.md).

### Delete and update

Segments are immutable. A delete marks the document ID in a per-segment *deletion bitmap* (Lucene
calls the complement "live docs"), and every query skips marked documents. An update is a delete
plus an insert of the new version into the newest segment. Merges drop deleted documents for good.
This is the same pattern as an [LSM tree](../lsm-trees/lsm-tree.md), and as a
[deletion vector](../analytical-storage/deletion-vector.md).

### Variants

- **Non-positional index:** store only document IDs (and maybe frequencies). Much smaller, but
  cannot answer phrase queries.
- **PostgreSQL GIN:** the keys are the elements of a value (words of a `tsvector`, elements of an
  array), stored in a B-tree. A key's row IDs are kept inline as a posting list when there are few,
  and in a separate B-tree (a posting tree) when there are many. With `fastupdate`, new entries
  first go to an unsorted pending list that is merged into the main structure in bulk.
- **Document-at-a-time versus term-at-a-time:** the iterators above walk all lists together, one
  document at a time. Term-at-a-time processing reads one list fully before the next and adds into
  per-document score accumulators. Dynamic pruning algorithms such as WAND skip documents that
  cannot reach the current top `k`.

## Invariants

- The term dictionary is sorted, with no duplicate terms.
- Each posting list is strictly increasing in document ID, and every ID is below the segment's
  document count.
- A term's document frequency equals the length of its posting list.
- Within one posting, positions are non-decreasing, and the stored term frequency equals the number
  of positions.
- For each skip entry, the entry it points to is the first one after the recorded document ID.
- A document is in the posting list of term `t` if and only if the analyser produced `t` for that
  document.
- No query returns a document marked as deleted.

## Implementation pitfalls

- **Different analysis at index and query time.** If the query string is not tokenised, lower-cased
  and normalised exactly as the documents were, terms silently fail to match. Use one analyser
  type for both, and include its configuration in the index metadata.
- **The first gap.** Decide whether the first document ID is stored as a gap from zero or as an
  absolute value, and whether positions restart at zero per document. A mismatch decodes every list
  off by a constant.
- **Positions can repeat.** Analysers that inject synonyms put two terms at the same position, so a
  position gap of zero is legal even though a document-ID gap of zero is not.
- **`advance` semantics.** `advance(target)` must return the first document at or after `target`,
  and must not move backwards if the iterator is already past it. Off-by-one errors here drop the
  match at exactly `target`.
- **Variable-byte decoding of bad input.** A corrupt file can contain a run of continuation bytes
  that shifts past 64 bits. In this crate, clippy denies shifts and additions that can overflow, so
  use `checked_shl` and stop with an error after ten bytes for a `u64`.
- **Renumbering on merge.** When merging segments, document IDs and deletion bitmaps must be
  remapped together; forgetting the deletions resurrects deleted documents.
- **Unicode.** Lower-casing and normalisation are not byte operations, and term order in the
  dictionary must be the byte order of the encoded terms, not a locale's collation.

## Testing

- **Brute-force model.** Keep the original documents in a `Vec`, and answer every query by scanning
  and tokenising all of them with the same analyser. Compare term, AND, OR, NOT and phrase queries
  against the index, as sets of document IDs.
- **Encoding round trips.** Property-test every codec (variable-byte, bit-packing, PFOR) on random
  gap lists, including lengths 0, 1, one less than the block size, the block size and one more, and
  values at the type's maximum.
- **Skip correctness.** For random targets, check `advance(target)` against a linear scan of the
  decoded list.
- **Segments and merges.** Index the same documents with flushes at random points, merge in random
  orders, delete some documents in between, and check the query results never change.
- **Corrupt input.** Fuzz the segment reader with truncated and random bytes; it must return errors,
  never panic.

## Further reading

- [Inverted Files for Text Search Engines](https://doi.org/10.1145/1132956.1132959), Zobel and
  Moffat, 2006.
- Manning, Raghavan and Schütze, *Introduction to Information Retrieval*, Cambridge University
  Press, 2008. Chapters 1 to 5 cover index construction and compression; the book is free online.

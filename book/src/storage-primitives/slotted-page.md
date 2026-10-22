# Slotted page

A fixed-size page (commonly 4 to 16 KiB) holding variable-length records. A header and an array of
slots grow from the front, record bytes grow from the back, and free space sits between them.
Records are addressed by `(page, slot)`, so they can move within the page without breaking
references.

- **Used by:** PostgreSQL heap and index pages, SQLite B-tree pages, InnoDB pages.
- **Cost:** `O(1)` lookup by slot. Deletions leave holes that need in-page compaction.
- **Notes:** the building block of every page-based structure below.

## How it works

### Layout

A page is a fixed-size byte array, for example 4096 bytes, that is read and written as a unit. It
has three regions:

- a **header** at the start, with at least the number of slots and two offsets, `lower` (the end of
  the slot array) and `upper` (the start of the record area);
- a **slot array** right after the header, growing towards the end of the page, with one entry per
  record holding its offset and length within the page;
- a **record area** at the end of the page, growing towards the start.

<figure class="diagram">
{{#include figures/slotted-page-layout.svg}}
<figcaption>Each slot holds the offset and length of its record. Slots grow to the right from the header, records grow to the left from the end of the page, and the free space between them is <code>upper - lower</code> bytes.</figcaption>
</figure>

Records are placed from the end of the page, so the first record inserted sits at the very end.

A record is identified by its slot number, and from outside the page by the pair
`(page number, slot number)`, called a record ID (PostgreSQL calls it a CTID). Because the slot
holds the offset, a record can move within the page, for compaction or because it grew, and only its
slot changes; every reference to it stays valid.

A slot can be in use or empty. An empty slot keeps its position in the array so that the slots
after it keep their numbers.

### Lookup

1. Check that the slot number is below the slot count.
2. Read the slot. If it is empty, the record does not exist.
3. Return the bytes from `offset` to `offset + length`.

### Insert

1. Decide which slot to use: an empty slot if one exists and may be reused (see the pitfalls), or a
   new one at the end of the array.
2. Compute the space needed: the record's length, plus the size of one slot if a new slot is
   needed.
3. If `upper - lower` is at least that, there is room. Otherwise, if the total free space,
   counting holes left by deletions, is enough, compact the page first. Otherwise the page is full,
   and the caller has to try another page.
4. Move `upper` down by the record's length and copy the record to `upper`.
5. Write the slot with the offset `upper` and the length; if the slot is new, increase the slot
   count and move `lower` up by one slot.
6. Return the slot number.

<figure class="diagram">
{{#include figures/slotted-page-insert.svg}}
<figcaption>Inserting a fourth record takes space from both ends of the free gap: its bytes below <code>upper</code>, and one new slot above <code>lower</code>.</figcaption>
</figure>

### Delete

1. Mark the slot as empty. Do not shift later slots, which would renumber them.
2. The record's bytes become a hole. Either compact now, or leave the hole and let a later insert
   trigger compaction.
3. If the slot was the last in the array, the array can shrink by one, and so can any empty slots
   that are now at the end.

### Update

1. If the new value is no longer than the old one, overwrite it in place and update the length.
2. Otherwise, free the old bytes and insert the new value as in steps 2 to 4 of insert, keeping the
   same slot number.
3. If the page does not have room, the record has to move to another page. To keep its record ID
   valid, leave behind a forwarding entry that holds the new record ID, or have the caller update
   every reference.

### Compaction

1. Collect the live slots and sort them by offset, highest first.
2. Set a cursor to the end of the page. For each slot, move the record so that it ends at the
   cursor, set the slot's offset to its new position, and move the cursor down by its length.
3. Set `upper` to the cursor. All free space is now one contiguous gap between `lower` and `upper`.

Processing records from the highest offset down means each one moves towards the end of the page
and never overwrites a record that has not moved yet.

<figure class="diagram">
{{#include figures/slotted-page-compact.svg}}
<figcaption>Deleting rec 1 empties slot s1 and leaves a hole that <code>upper - lower</code> does not count. Compaction slides rec 2 up against rec 0 and rewrites only s2's offset; s1 stays empty, so s2 keeps its number.</figcaption>
</figure>

### Variants

- **PostgreSQL:** the default page is 8 KiB, with a 24-byte header that includes `pd_lower`,
  `pd_upper`, `pd_special` and the page LSN. Each line pointer is 4 bytes: a 15-bit offset, a 15-bit
  length and 2 bits of state (unused, normal, redirect or dead). The end of the page can hold a
  "special space" that index pages use for their own metadata.
- **SQLite:** each cell pointer is 2 bytes, and the array is kept in key order, so a B-tree page can
  binary search its slots while the cells themselves sit in any order. Freed space is kept as a
  linked list of freeblocks of at least 4 bytes; smaller gaps are counted as fragmented bytes, of
  which a well-formed page has at most 60. Page sizes are powers of two from 512 to 65536 bytes.
- **Sorted slot arrays:** for [B+ tree](../page-based-trees/b-plus-tree.md) nodes, inserting a key
  shifts the slots after it, which renumbers them. That is acceptable for index entries, which
  nothing references by slot number, but not for table rows.
- **Overflow pages:** a record too large for one page is split, with a prefix in the page and the
  rest in a chain of overflow pages.

## Invariants

- `header size ≤ lower ≤ upper ≤ page size`.
- `lower = header size + slot count · slot size`.
- Every live slot's range `[offset, offset + length)` lies within `[upper, page size)`, and no two
  live ranges overlap.
- Free space adds up: `page size - lower - (sum of live record lengths)` equals
  `upper - lower` plus the bytes in holes.
- Inserting, deleting, updating and compacting never change the slot number of any other live
  record, nor its bytes.
- A page serialises to exactly `page size` bytes, and decoding those bytes gives back the same
  records under the same slot numbers.

## Implementation pitfalls

- **Reusing a slot that something still points to.** If an index still holds the record ID of a
  deleted record, and a new record takes the same slot, the index now points at the wrong row.
  PostgreSQL marks such line pointers dead and makes them reusable only after VACUUM has removed
  the index entries that point at them.
- **Forgetting the slot in the free-space check.** A new record needs its length plus a slot. A
  page that checks only the record length overflows `lower` past `upper` on the last insert.
- **Offsets that do not fit their field.** A 2-byte offset cannot hold 65536. SQLite stores 0 to
  mean 65536 for the content start of a 64 KiB page, and PostgreSQL's 15-bit offsets limit its pages
  to 32 KiB. Choose field widths from the largest page size you allow.
- **Confusing an empty record with an empty slot.** A zero-length record is legal. Mark empty slots
  with a flag rather than with length 0, or forbid empty records explicitly.
- **Panicking on a corrupt page.** A page read from disk can contain any bytes. Clippy denies slice
  indexing in this crate, so read fields with `get(range)` and `from_le_bytes`, and turn an offset
  or length that points outside the page into an error. `copy_within` and `split_at_mut` panic on
  bad ranges, so validate ranges before calling them, or use `split_at_mut_checked`.
- **Serialising by casting structs.** In C, a page header is often a struct cast over the buffer.
  `unsafe` is forbidden in this crate, and struct layout and endianness are not portable anyway.
  Encode every field explicitly as little-endian bytes at a fixed offset.
- **Arithmetic on offsets.** `upper - length` underflows if the free-space check is wrong. Use
  `checked_sub` so the bug surfaces as an error, which clippy requires here in any case.
- **Fragmentation that is never reclaimed.** If inserts check only `upper - lower`, holes are never
  reused and the page reports itself full while mostly empty. Track total free space separately and
  compact when it suffices.

## Testing

- Reference model: keep a `BTreeMap<slot, Vec<u8>>` alongside the page. Apply random inserts,
  deletes and updates of random sizes to both, and after every operation assert that every slot
  reads back the same, that the free space the page reports matches the model, and that the
  invariants above hold.
- Fill and churn: fill a page with small records until it reports full, delete every other one,
  then insert records that only fit after compaction. Check that every surviving record keeps its
  slot number and bytes.
- Round trip: serialise the page after every operation, decode it, and compare.
- Hostile input: decode random byte arrays as pages, for example with `proptest` or a fuzzer. Each
  must give an error or a page that satisfies the invariants, and must never panic.
- Edge cases: a zero-length record, a record that fills the page exactly, a record one byte too
  large, deleting the last slot, and the smallest and largest page sizes allowed.

## Further reading

- Petrov, *Database Internals*, O'Reilly, 2019, chapter 3, "File Formats": slotted pages, cell
  layout and variable-size data.
- [The Internals of PostgreSQL, chapter 1](https://www.interdb.jp/pg/pgsql01.html), Suzuki. A
  PostgreSQL heap page, with line pointers as the slot array.
- [SQLite database file format](https://sqlite.org/fileformat2.html). The byte layout of SQLite's
  B-tree pages, cell pointer arrays and freeblocks.

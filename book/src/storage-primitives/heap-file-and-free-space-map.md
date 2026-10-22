# Heap file and free space map

> Builds on [slotted page](../storage-primitives/slotted-page.md).

An unordered collection of pages holding a table's rows, plus a small structure that records roughly
how much free space each page has, so inserts can find room without scanning.

- **Used by:** PostgreSQL heap tables and its free space map (FSM).
- **Cost:** `O(1)` insert when the FSM finds a page. Full scans read every page.

## How it works

### Layout

A heap file is a file divided into fixed-size [slotted pages](slotted-page.md), numbered from 0.
Rows go into whichever page has room, in no particular order, which is what "heap" means here. A
row is addressed by its record ID, `(page number, slot number)`, and indexes store record IDs to
point at rows.

The free space map (FSM) answers one question: which page has at least `n` free bytes? It does not
need to be exact, only good enough to avoid reading pages that turn out to be full. PostgreSQL
stores one byte per heap page, the page's free space divided by `page size / 256` and rounded down,
so each step is 32 bytes for an 8 KiB page and the value ranges from 0 to 255.

To find a page quickly, the bytes are the leaves of a binary tree in which each internal node holds
the maximum of its two children. The root then holds the largest free space in the whole file.

<figure class="diagram">
{{#include figures/heap-file-and-free-space-map-tree.svg}}
<figcaption>An FSM over four heap pages. Each leaf is its page's free-space category, each internal node is the maximum of its two children, and the bracketed numbers are the nodes' positions in the array.</figcaption>
</figure>

The tree is stored as an array, with the children of node `i` at `2i + 1` and `2i + 2`. PostgreSQL
keeps such a tree inside each FSM page and stacks FSM pages three levels deep, where a leaf in
an upper level stands for the root of a page below. Its FSM lives in a separate file next to the
table, with the suffix `_fsm`.

### Search for a page

1. Convert the space needed into a category, rounding up: `ceil(needed / step)`.
2. If the root is below that category, no page qualifies.
3. Otherwise descend: at each node, go to a child whose value is at least the category. If both
   qualify, either will do.
4. The leaf reached gives the page number.

<figure class="diagram">
{{#include figures/heap-file-and-free-space-map-search.svg}}
<figcaption>A search for category 5. The right subtree's maximum is 4, so the search never looks below it, and the left subtree leads to page 1.</figcaption>
</figure>

PostgreSQL also keeps a hint in each FSM page, `fp_next_slot`, saying where the previous search
ended, and starts the next search from there. That spreads concurrent inserters across pages while
still filling the file roughly in order.

### Update after a change

1. Compute the page's new category from its actual free space, rounding down.
2. Write it into the page's leaf.
3. Walk up the tree, setting each parent to the maximum of its two children. Stop at the root, or
   as soon as a parent's value does not change.

<figure class="diagram">
{{#include figures/heap-file-and-free-space-map-update.svg}}
<figcaption>Page 1 fills up and its category drops from 7 to 2. The new maximum climbs one level at a time, and the root now reports 4, the free space of page 2.</figcaption>
</figure>

### Insert a row

1. Search the FSM for a page with room for the row plus one slot.
2. If one is found, read the page and try the insert. The FSM can be out of date, so the page may
   not have room after all. In that case, update its leaf with the real value and search again.
3. If no page qualifies, extend the file with a new empty page, insert there, and add a leaf for
   it.
4. Update the FSM entry of the page that received the row.
5. Return the record ID.

### Delete a row

1. Read the page named by the record ID and mark the slot empty.
2. Update that page's FSM entry. PostgreSQL leaves this to VACUUM, which first has to make sure no
   transaction can still see the row; a heap without multiversioning can update it immediately.

### Update a row

1. If the new version fits in the same page, update it there, and the record ID stays the same.
2. Otherwise insert it into another page found through the FSM. Either leave a forwarding entry at
   the old record ID, or delete the old row and update every index that pointed to it.

### Scan

1. For each page number from 0 to the last page, read the page.
2. Yield every live slot with its record ID.

### Variants

- **Page lists:** the textbook alternative keeps two linked lists of pages, those with free space
  and those that are full, threaded through the page headers. It is simpler, but finding a page
  with room for a large row may walk the whole list.
- **Page directory:** a few pages near the start of the file hold an array of
  `(page number, free bytes)` entries. A search is a linear scan of the directory, which is still
  far cheaper than scanning the heap.
- **Fill factor:** inserts stop using a page before it is full, keeping room for later updates to
  stay on the same page. PostgreSQL exposes this as the table's `fillfactor` setting.

## Invariants

- Every heap page has exactly one FSM leaf, and the FSM has no leaves beyond the last page.
- Every internal FSM node equals the maximum of its children.
- When the FSM is updated together with the page, each leaf is at most the page's actual free space
  divided by the step and rounded down. A search therefore never returns a page without room.
- A record ID returned by insert reads back the same row until that row is deleted or moved.
- A scan yields every live row exactly once.

## Implementation pitfalls

- **Trusting the FSM.** In PostgreSQL the FSM is not WAL-logged, and after a crash it can claim
  space on a page that never reached disk; it detects that by comparing against the file's actual
  size. Any FSM that can be stale needs the same defence: check the page itself, and correct the
  leaf when it was wrong.
- **Rounding in the wrong direction.** Round the stored value down and the requested value up.
  Rounding the stored value up advertises space a page does not have; rounding the request down
  asks for less than the row needs.
- **Forgetting the slot.** The space a row needs includes its new slot, unless it reuses an empty
  one.
- **Every inserter on the same page.** A search that always returns the leftmost qualifying leaf
  sends all concurrent inserts to one page, which then becomes a point of contention. Start from a
  rotating hint, as PostgreSQL does.
- **Extending the file.** Two threads that both find no room may both add a page, and a crash
  between adding a page and recording it in the FSM leaves a page the FSM does not know about. On
  open, compare the FSM's size with the heap file's and rebuild the missing leaves from the pages.
- **A tree whose leaf count is not a power of two.** Pad the array with leaves of value 0, or
  handle a missing right child explicitly. Compute child and parent indexes with `checked_mul` and
  `checked_add`, which clippy requires in this crate, and read nodes with `get`.
- **Page numbers in record IDs.** Give page numbers their own type, such as a `PageId(u32)`
  newtype, and slot numbers another, so the two cannot be swapped in a record ID.

## Testing

- Reference model: keep a `HashMap<RecordId, Vec<u8>>` alongside the heap. Apply random inserts,
  deletes and updates of varied sizes, and after every operation check that every record ID reads
  back its row and that a scan yields exactly the model's rows.
- FSM structure: after every operation, assert that every internal node is the maximum of its
  children and that every leaf matches its page's free space.
- FSM search against brute force: for random trees and requests, compare the search with a linear
  scan of the leaves. The search must return a qualifying page if and only if one exists.
- Stale FSM: overwrite leaves with wrong values, then insert. Inserts must still succeed, and must
  repair the leaves they found to be wrong.
- Persistence: fill several pages, reopen the files, and check that an FSM rebuilt from the pages
  equals the one stored.
- Edge cases: an empty heap, a row that needs a fresh page, a row larger than any page can hold, and
  a heap whose page count crosses a power of two.

## Further reading

- [PostgreSQL documentation: Free Space
  Map](https://www.postgresql.org/docs/current/storage-fsm.html). One byte per heap page, kept in a
  small tree of pages.
- [The Internals of PostgreSQL, chapter 1](https://www.interdb.jp/pg/pgsql01.html), Suzuki. How
  tuples are written to and read from heap pages.

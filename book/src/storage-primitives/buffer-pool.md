# Buffer pool

> Builds on [slotted page](../storage-primitives/slotted-page.md).

A fixed set of in-memory frames caching disk pages. Callers pin a page while using it, dirty pages
are written back before their frame is reused, and an eviction policy picks the victim.

- **Used by:** every page-based database. PostgreSQL uses clock sweep; InnoDB uses an LRU list with
  a midpoint insertion point so that scans do not flush the hot pages.
- **Cost:** `O(1)` hit through a hash table from page ID to frame.
- **Notes:** eviction policies worth comparing: LRU, clock, LRU-K (O'Neil et al., 1993), 2Q, and
  TinyLFU admission (which uses a [count-min sketch](../sketches/count-min-sketch.md)).

## How it works

### Layout

A buffer pool owns a fixed number of frames, each a page-sized byte buffer, allocated once at
start-up. Around them it keeps:

- a **page table**, a hash map from page ID to the frame holding that page;
- per frame, the **page ID** it holds, a **pin count** (how many callers are using it right now), a
  **dirty flag** (whether it changed since it was read), and whatever state the eviction policy
  needs;
- a **free list** of frames that hold no page;
- a **replacer**, the eviction policy, which picks a victim among frames whose pin count is 0.

<figure class="diagram">
{{#include figures/buffer-pool-layout.svg}}
<figcaption>A pool of three frames. Frame 0 holds page 17 and is pinned by two callers; frame 1 holds page 3, unpinned and dirty, so it is the only eviction candidate; frame 2 is free.</figcaption>
</figure>

A caller never reads the file directly. It asks the pool for a page, gets back access to a frame,
and must release it when done.

### Fetch a page

1. Look the page ID up in the page table. On a hit, increase the frame's pin count, tell the
   replacer the page was accessed, and return the frame.
2. On a miss, take a frame from the free list. If the list is empty, ask the replacer for an
   unpinned victim. If every frame is pinned, fail: the pool is exhausted.
3. If the victim is dirty, write it back first. With a [write-ahead log](write-ahead-log.md), make
   sure the log is flushed up to the victim's page LSN before writing the page.
4. Remove the victim's page ID from the page table.
5. Read the requested page from disk into the frame. Set its page ID, a pin count of 1 and a clean
   dirty flag, add it to the page table, and tell the replacer.
6. Return the frame.

<figure class="diagram">
{{#include figures/buffer-pool-fetch-miss.svg}}
<figcaption>Fetching page 9 after page 5 has taken the last free frame. The replacer picks frame 1, the only unpinned one; its dirty page 3 is written back before page 9 is read into it.</figcaption>
</figure>

### Release a page

1. Decrease the pin count.
2. If the caller changed the page, set the dirty flag. Never clear it here: another caller may have
   dirtied it.
3. When the pin count reaches 0, the frame becomes a candidate for eviction.

### Create a page

1. Allocate a new page ID, usually by extending the file.
2. Get a frame as in steps 2 to 4 of fetch, fill it with zeros instead of reading, and mark it
   dirty, since the page does not exist on disk yet.

### Flush

1. To flush one page, write it if it is dirty (respecting the log rule), then clear the dirty flag.
2. A checkpoint flushes every dirty frame. PostgreSQL also has a background writer that writes
   dirty pages ahead of time, so that a fetch rarely has to wait for a write-back.

### Eviction policies

- **LRU:** keep frames in a list ordered by last access, and evict from the cold end. A single
  large scan touches every page once and pushes the whole working set out, a problem called
  sequential flooding.
- **Clock:** the frames form a circle with one reference bit each, set on access. A hand sweeps
  the circle: a frame with the bit set has it cleared and is skipped, and the first unpinned frame
  with the bit clear is the victim. It approximates LRU without moving list nodes on every hit.
- **Clock sweep with usage counts:** PostgreSQL's variant replaces the bit with a small counter,
  increased on each pin up to a maximum of 5 and decreased by the sweeping hand. A frame is evicted
  when the hand finds it unpinned with a count of 0.
- **LRU-K:** evict the page whose K-th most recent access is oldest, counting a page with fewer
  than K accesses as infinitely old. A page touched once by a scan then loses to one touched
  twice. It keeps access history even for pages no longer in the pool.
- **2Q:** new pages enter a small FIFO queue. Only pages accessed again after leaving it, which a
  "ghost" queue of recently evicted page IDs detects, enter the main LRU queue.
- **Midpoint insertion (InnoDB):** one LRU list split into a new and an old sublist, with the old
  sublist taking 3/8 of the pool by default. A page read from disk enters at the head of the old
  sublist, the midpoint, and moves to the head of the new sublist when it is accessed there. The
  setting `innodb_old_blocks_time` can delay that move, so that the accesses of one scan do not
  promote its pages.
- **Admission with TinyLFU:** separate from eviction, an admission filter estimates how often the
  candidate and the victim are accessed, using a count-min sketch, and keeps the victim when it is
  the more frequent of the two.
- **Ring buffers for scans:** PostgreSQL gives large sequential scans, VACUUM and bulk writes a
  small private ring of frames, so they recycle their own frames instead of evicting the rest of
  the pool.

<figure class="diagram">
{{#include figures/buffer-pool-clock.svg}}
<figcaption>One clock sweep. The hand passes the pinned frame 0, clears the reference bits of frames 1 and 2, which were used since its last pass, and stops at frame 3, whose bit was already clear.</figcaption>
</figure>

### Why not `mmap`

Mapping the file into memory hands caching to the operating system, which may write a dirty page
back at any moment, before the log records describing it are durable. Crotty, Leis and Pavlo also
describe I/O stalls on page faults that the database cannot schedule around, and I/O errors that
arrive as signals instead of return values.

## Invariants

- The page table and the frames agree: a page ID maps to frame `f` if and only if frame `f` holds
  that page ID. No page is in two frames.
- Frames on the free list are not in the page table, and have a pin count of 0.
- A frame with a pin count above 0 is never chosen as a victim, and its page ID never changes.
- The pin count of every frame equals the number of outstanding, unreleased fetches of its page.
- A dirty page is written back before its frame is reused, and never written while
  `page LSN > flushed LSN`.
- After flushing every page, no frame is dirty and the file holds the same bytes as the frames.
- The number of frames never changes.

## Implementation pitfalls

- **Pin leaks.** A caller that forgets to release a page keeps its frame forever, and the pool
  eventually reports itself exhausted. In Rust, return a guard that releases the pin in its `Drop`
  implementation, so that every path, including early returns with `?`, releases it.
- **Losing the dirty flag.** Releasing with `dirty = false` must not clear a flag set by another
  caller. OR the flags together.
- **Two frames for one page.** If two threads miss on the same page at once, both may read it into
  different frames, and their changes diverge. Insert a placeholder into the page table before
  starting the read, and make the second thread wait for it.
- **Disk I/O under the page table lock.** Holding the lock that protects the page table during a
  read or write serialises every fetch behind one disk operation. Release it before the I/O and
  re-check afterwards.
- **A failed write-back.** If writing a dirty victim fails, the frame must stay dirty and keep its
  page; reusing it loses the change. Return the error.
- **Writing in `Drop`.** A guard's `Drop` cannot return an error, and dropping a `File` ignores
  close errors. Keep `Drop` to releasing the pin; do write-backs in methods that return `Result`.
- **Borrowing frames without `unsafe`.** Handing out `&mut [u8]` into a frame through `&self` needs
  interior mutability, and `unsafe` is forbidden in this crate. A `RwLock` per frame works: the
  guard holds the lock guard, borrowed from the pool, so its lifetime ties it to the pool. Do not
  hold a `std::sync` lock guard across an `.await` if the pool is used from async code.
- **Page offsets.** The file offset of a page is `page ID · page size`. In this crate that is a
  `checked_mul`, as clippy denies arithmetic that can overflow, and the read is `read_exact_at`, so
  a short read near the end of the file is an error rather than a page of stale bytes.
- **Counters that overflow.** Clock usage counts and LRU-K timestamps grow without bound in a long
  run. Cap them with `saturating_add` or `min`, as PostgreSQL caps its usage count.
- **Unbounded history.** LRU-K and 2Q remember pages that have left the pool. Bound that history,
  or memory grows with the size of the file rather than the pool.

## Testing

- Reference model: keep a `HashMap<PageId, Vec<u8>>` as the truth. With a pool much smaller than
  the number of pages, run random fetches, writes, releases and flushes, and assert that every
  fetched page equals the model. Every page must survive any number of evictions.
- Policy unit tests: replay short access sequences whose victims can be worked out by hand, and
  check each victim. For example, LRU with three frames and accesses `1, 2, 3, 1, 4` must evict
  page 2.
- Pinning: pin every frame, and assert that the next fetch fails while those pages stay readable;
  release one and assert that the fetch succeeds.
- Write-back accounting: put the disk behind a test double that counts writes. Clean pages must
  never be written; a dirty page must be written exactly once before its frame is reused.
- Log rule: give the pool a fake log with a controllable flushed LSN, and assert that no page is
  ever written while its page LSN is above it.
- Concurrency: run random workloads from several threads and check the invariants afterwards.
  Model-check small cases, such as two threads missing on the same page, with `loom`.
- Scan resistance: mix a small hot set with a long sequential scan, and compare hit rates across
  policies. LRU should do worst.

## Further reading

- [Are You Sure You Want to Use MMAP in Your Database Management
  System?](https://www.cidrdb.org/cidr2022/papers/p13-crotty.pdf), Crotty, Leis and Pavlo, 2022. Why
  databases manage their own buffer pool instead of relying on the operating system's page cache.
- [The LRU-K Page Replacement Algorithm for Database Disk
  Buffering](https://www.cs.cmu.edu/~christos/courses/721-resources/p297-o_neil.pdf), O'Neil, O'Neil
  and Weikum, 1993.
- [2Q: A Low Overhead High Performance Buffer Management Replacement
  Algorithm](https://www.vldb.org/conf/1994/P439.PDF), Johnson and Shasha, 1994.
- [TinyLFU: A Highly Efficient Cache Admission Policy](https://arxiv.org/abs/1512.00727), Einziger,
  Friedman and Manes.
- [The Internals of PostgreSQL, chapter 8: Buffer Manager](https://www.interdb.jp/pg/pgsql08.html),
  Suzuki. Clock sweep and the ring buffers PostgreSQL uses for bulk reads.

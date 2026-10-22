# Columnar segments

Each column of a table is stored contiguously, in row groups of fixed size. Queries read only the
columns they need, and runs of similar values compress well.

- **Used by:** Parquet, ORC, ClickHouse MergeTree parts, DuckDB row groups.
- **Notes:** the PAX layout keeps column-wise data inside row-wise pages, a hybrid of the two.

## How it works

### Layout

A segment is one immutable file (or one directory of files) holding a horizontal slice of a table.
Its vocabulary comes from Parquet, and most columnar formats have an equivalent for each term:

- **Row group:** a range of consecutive rows. A segment holds one or more of them.
- **Column chunk:** the values of one column within one row group, stored contiguously.
- **Page:** the unit of encoding and compression inside a column chunk. A reader has to decode a
  whole page to get any value in it.
- **Footer:** the metadata that says where every column chunk starts, how long it is, how it is
  encoded, and what its statistics are. It is written last, so the writer never has to seek back.

<figure class="diagram">
{{#include figures/columnar-segments-layouts.svg}}
<figcaption>The same three rows stored row by row and column by column. In the column layout a query that needs only <code>price</code> reads one contiguous run.</figcaption>
</figure>

<figure class="diagram">
{{#include figures/columnar-segments-file.svg}}
<figcaption>A segment with two row groups. Column b is dictionary-encoded, so its chunks start with a dictionary page. The footer records where every column chunk starts (blue), and a reader finds the footer from the length stored just before the closing magic (teal).</figcaption>
</figure>

Parquet is exactly this shape: the file starts and ends with the magic bytes `PAR1`, and the last
eight bytes are the footer length followed by the magic. The footer is a Thrift structure, and each
page has its own small header. ORC uses the same idea with different names: row groups are
"stripes", each with its own index and footer, and the file ends with a postscript whose length is
stored in the very last byte.

The per-column-chunk statistics in the footer are [zone maps](zone-maps.md). They are what lets a
reader skip a row group without reading it.

### Choosing sizes

Two sizes trade against each other:

- **Row group size** sets the unit of skipping and of parallelism. Larger row groups give longer
  sequential reads and better compression, but the writer has to buffer a whole row group before
  it can write it, and a zone map over more rows skips less. Parquet's documentation recommends
  large row groups (512 MB to 1 GB) for HDFS; ORC stripes are typically around 200 MB.
- **Page size** sets the unit of decoding. Smaller pages make point reads cheaper; larger pages
  have less header overhead. Parquet recommends 8 KB pages.

For a first implementation, cap row groups by a row count (for example 64 Ki rows) and pages by
an encoded byte size. Both limits belong in the writer's configuration, not in the format.

### Representing NULLs

NULLs are not stored among the values. Each column chunk carries a separate validity stream, one
entry per row, and the value stream holds only the non-NULL values. Parquet calls these entries
definition levels; for a flat, nullable column they are 0 (NULL) or 1 (present) and are
run-length encoded, so a column of 1,000 NULLs costs a single run. An in-memory batch uses the
same split, a validity bitmap beside the values (see [columnar
batch](../query-execution/columnar-batch.md)).

### Write

The writer takes rows (or batches) in order and produces the file in one pass:

1. Write the magic bytes.
2. Buffer incoming rows column by column until the row group reaches its size limit, or the input
   ends.
3. For each column of the buffered row group:
   1. Split its values into pages by encoded size.
   2. For each page, choose an encoding (see [column encodings](column-encodings.md)), encode the
      validity stream and the values, optionally compress the result, and write a page header
      followed by the bytes.
   3. While encoding, fold every non-NULL value into the chunk's min, max and null count.
   4. Record the chunk's starting offset, total length, encodings and statistics.
4. Record the row group's row count, clear the buffers and continue with the next row group.
5. When the input ends, serialise the footer, write it, then write its length and the magic.
6. Flush and sync the file, then make it visible under its final name with an atomic rename. From
   then on the file is never modified.

### Open

1. Read the last eight bytes. Check the magic, and decode the footer length.
2. Check that the footer length fits inside the file (`length + 8 + magic size <= file size`).
3. Read and parse the footer. Validate every offset and length in it against the file size before
   trusting it.

### Scan with projection and predicate pushdown

A scan names the columns it returns and, optionally, a predicate such as `price > 100`.

1. Collect the columns needed: the projected ones plus those referenced by the predicate.
2. For each row group, test the predicate against the footer statistics of its columns. If no row
   can match, skip the row group without any I/O.
3. For each surviving row group, read and decode the predicate columns first, evaluate the
   predicate, and build a selection vector of the rows that passed.
4. If the selection is empty, move to the next row group. Otherwise read only the projected
   columns, and decode or copy only the selected positions. This is called late materialisation.
5. Emit the result in batches.

<figure class="diagram">
{{#include figures/columnar-segments-scan.svg}}
<figcaption>Scanning <code>SELECT name WHERE price &gt; 100</code>. Row group 0 is skipped from its footer statistics alone. In the other two, the <code>price</code> chunk is read and filtered first, then only the selected rows of <code>name</code> are fetched; <code>id</code> and <code>qty</code> are never read.</figcaption>
</figure>

Readers usually issue one read per needed column chunk (Parquet calls the column chunk the unit of
I/O), so a query touching 3 of 100 columns reads roughly 3% of the bytes.

### Reading by row number

To fetch row `r`, find the row group containing it from the cumulative row counts, then the page
containing it. That second step needs the first row number of every page, which a plain footer
does not store. Parquet adds it as an optional "offset index" (with a per-page "column index" of
min and max values) stored near the footer. A [sparse primary index](sparse-primary-index.md)
serves the same purpose for sorted data.

### Variants

- **One file per column.** Wide ClickHouse MergeTree parts store each column in its own `.bin`
  file with a mark file per column, instead of interleaving column chunks in one file.
- **Segments inside a database file.** DuckDB keeps row groups of all tables in its single database
  file and tracks them in its own metadata, rather than one file per segment.
- **Nested data.** Dremel and Parquet shred nested and repeated fields into columns, adding
  repetition levels to the definition levels so records can be reassembled.
- **PAX.** Pages of a row store, each laid out column by column internally. It gains cache
  locality for scans while keeping a row's fields on the same page.

## Invariants

- Every column chunk in a row group describes the same number of rows, NULLs included, and that
  number equals the row group's row count.
- The sum of the row group row counts equals the number of rows written.
- Column chunks lie inside the data region, do not overlap, and the footer's offsets point at page
  headers.
- For every column chunk with statistics, `min <= v <= max` for each non-NULL value `v`, and the
  null count equals the number of NULLs.
- Reading every column of every row group returns the rows that were written, in the same order.
- Once the file is visible under its final name, its bytes never change.

## Implementation pitfalls

- **Trusting lengths from disk.** Footer length, page sizes and offsets come from the file and may
  be corrupt. Check them against the file size with `checked_add` before slicing or allocating;
  an unchecked length becomes a huge allocation or an out-of-bounds slice. In this crate clippy
  denies `v[a..b]`, so use `get(a..b)` and turn `None` into an error.
- **Crash before the footer.** A file without a valid footer is unreadable, because nothing else
  says where the chunks are. Write to a temporary name and rename only after the footer is synced,
  so a reader never sees a half-written segment.
- **Row counts that disagree.** It is easy to count only non-NULL values for a column and get a
  shorter chunk than its neighbours. Count NULLs as rows, and assert the per-column counts match
  before closing the row group.
- **Endianness and widths.** Write every integer with an explicit byte order (`to_le_bytes`), and
  convert on-disk `u64` offsets with `usize::try_from` rather than `as`.
- **Buffer size measured in rows.** A row group of very wide strings can exhaust memory long before
  it reaches its row limit. Track buffered bytes and flush on whichever limit comes first.
- **Empty input.** A segment with zero rows should still have a valid footer with zero row groups.
  Do not write a row group with zero rows; readers then have to handle empty statistics.
- **Statistics for empty or all-NULL chunks.** There is no min or max. Write them as absent, never
  as zero (see [zone maps](zone-maps.md)).

## Testing

- Round trip: generate a random schema and random rows (with NULLs), write a segment, read it back
  and compare. A property test over many schemas and row counts finds most layout bugs.
- Projection: reading a subset of columns returns the same values as reading everything and then
  projecting.
- Pushdown against a full scan: for random predicates, the rows returned with row-group skipping
  and late materialisation equal the rows of a full scan filtered afterwards.
- Corruption: truncate a valid file at every length, and flip random bytes in the footer. Opening
  must return an error and never panic.
- Edge cases: zero rows, one row, a row count that is an exact multiple of the row group size and
  one more than it, an all-NULL column, a column with one distinct value, very long strings, and
  the minimum and maximum of every integer type.
- Interoperability, if you follow the Parquet layout: the
  [parquet-testing](https://github.com/apache/parquet-testing) repository holds files written by
  other implementations.

## Further reading

- [The Design and Implementation of Modern Column-Oriented Database
  Systems](https://ir.cwi.nl/pub/21772/1900000024-Abadi-Vol5-DBS-024.pdf), Abadi, Boncz,
  Harizopoulos, Idreos and Madden, 2013. The best single overview of column stores.
- [C-Store: A Column-oriented
  DBMS](https://www.vldb.org/archives/website/2005/program/paper/thu/p553-stonebraker.pdf),
  Stonebraker et al., 2005.
- [Dremel: Interactive Analysis of Web-Scale
  Datasets](https://static.googleusercontent.com/media/research.google.com/en//pubs/archive/36632.pdf),
  Melnik et al., 2010. Columnar storage for nested records, the basis of Parquet.
- [Parquet file format](https://parquet.apache.org/docs/file-format/). Row groups, column chunks,
  pages and footer metadata.

# Storage primitives

Everything a database stores eventually becomes bytes in a file, and the structures in this chapter
decide how. Two families start here. Log-structured designs only append to files and never change
bytes already written. Page-based designs divide files into fixed-size pages and change those pages
in place, caching them in memory.

- [Append-only log](append-only-log.md)
- [Write-ahead log](write-ahead-log.md)
- [Slotted page](slotted-page.md)
- [Heap file and free space map](heap-file-and-free-space-map.md)
- [Buffer pool](buffer-pool.md)
- [Ring buffer](ring-buffer.md)

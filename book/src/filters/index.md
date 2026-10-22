# Filters

Small, approximate membership structures. A filter can answer "definitely not here" without touching
the data, which lets an engine skip a disk read, an SSTable or a partition. They trade a few bits
per key and some false positives for that.

- [Bloom filter](bloom-filter.md)
- [Cuckoo filter](cuckoo-filter.md)
- [Xor and Ribbon filters](xor-and-ribbon-filters.md)
- [Range filter](range-filter.md)

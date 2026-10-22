# Sketches and statistics

Compact summaries of large data that answer questions approximately: how many distinct values, how
often a value occurs, what the 99th percentile is. Query optimisers use them to estimate
cardinalities, and analytical engines use them to answer `COUNT(DISTINCT)` and percentile queries
without holding all the data. Most can be merged, so partial results from many segments or machines
combine into one.

- [HyperLogLog](hyperloglog.md)
- [Theta sketch](theta-sketch.md)
- [Count-min sketch](count-min-sketch.md)
- [t-digest](t-digest.md)
- [KLL sketch](kll-sketch.md)
- [Reservoir sample](reservoir-sample.md)
- [Histogram](histogram.md)

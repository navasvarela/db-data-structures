# Analytical storage

Analytical (OLAP) queries read a few columns of a very large number of rows, so OLAP storage is laid
out column by column, compressed, and annotated with metadata that lets a scan skip data it does not
need. Files are mostly written once, which changes how updates and deletes have to work.

- [Columnar segments](columnar-segments.md)
- [Zone maps](zone-maps.md)
- [Bitmap index](bitmap-index.md)
- [Sparse primary index](sparse-primary-index.md)
- [Deletion vector](deletion-vector.md)
- [Delta and main store](delta-and-main-store.md)
- [Table snapshot manifest](table-snapshot-manifest.md)
- [Data cube and star-tree index](data-cube-and-star-tree-index.md)
- [Column encodings](column-encodings.md)
- [Floating-point and string compression](floating-point-and-string-compression.md)

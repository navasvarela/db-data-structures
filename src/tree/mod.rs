//! Ordered, in-memory search trees.
//!
//! Databases keep sorted keys in trees: the memtable of a log-structured merge tree, the pages of
//! a B-tree, an in-memory index that has to answer range queries. A hash index cannot do any of
//! that, because hashing throws away the order of the keys.

pub mod red_black;

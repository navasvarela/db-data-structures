// The README doubles as the crate documentation, so its examples run as doctests and cannot
// drift from the API.
#![doc = include_str!("../README.md")]

pub mod backend;
pub mod hash;
pub mod sstable;
pub mod tree;

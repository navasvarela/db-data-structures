use std::io::{self, Result};

pub mod file;
pub mod memory;

/// Bytes a record's length prefix occupies in [`FileByteStore`](file::FileByteStore).
///
/// Every backend reserves this much room in front of each record, so that all of them hand out
/// the same offsets for the same sequence of writes and agree on how much space a record takes.
pub(crate) const LENGTH_PREFIX_LEN: u64 = 4;

/// A `ByteStore` represents a simple storage primitive.
pub trait ByteStore {
    /// Read a record at a given byte offset
    fn read_record(&self, offset: u64) -> Result<Vec<u8>>;
    /// Write or overwrite a record at a given offset
    fn write_record_at(&self, offset: u64, data: &[u8]) -> std::io::Result<u64>;
    /// Append a record to the storage
    fn append(&self, data: &[u8]) -> std::io::Result<u64>;
    /// Every record in the store, in ascending offset order.
    ///
    /// Callers that replay the store to rebuild their state rely on this order to let a later
    /// record win over an earlier one.
    fn records(&self) -> impl Iterator<Item = Result<StoredRecord>>;
}

/// A record as read back from a [`ByteStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRecord {
    pub offset: u64,
    pub payload: Vec<u8>,
}

/// Space a record of `payload_len` bytes occupies, rejecting payloads whose length does not fit
/// the `u32` prefix that `FileByteStore` writes.
pub(crate) fn record_size(payload_len: usize) -> Result<u64> {
    let len = u32::try_from(payload_len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "record too large"))?;
    Ok(LENGTH_PREFIX_LEN + u64::from(len))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::empty(0, LENGTH_PREFIX_LEN)]
    #[case::small(5, LENGTH_PREFIX_LEN + 5)]
    #[case::largest(usize::try_from(u32::MAX).unwrap(), LENGTH_PREFIX_LEN + u64::from(u32::MAX))]
    fn record_size_counts_the_length_prefix(#[case] payload_len: usize, #[case] expected: u64) {
        assert_eq!(record_size(payload_len).unwrap(), expected);
    }

    #[test]
    fn record_size_rejects_payloads_longer_than_the_length_prefix_allows() {
        let too_long = usize::try_from(u64::from(u32::MAX) + 1).unwrap();

        assert_eq!(
            record_size(too_long).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

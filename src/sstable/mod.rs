use crate::backend::record::DecodeError;
use std::io;
use thiserror::Error;

/// Sorted String Table data structure. 
/// 
pub trait SSTable {
    fn get(&self, key: String) -> Option<String>;
    fn set(&mut self, key: String, value: String) -> Result<(), SSTableError>;
}

#[derive(Error, Debug)]
pub enum SSTableError {
    #[error("Error creating hash index: {0}")]
    IOError(#[from] io::Error),
    #[error("Corrupt record at offset {offset}: {source}")]
    CorruptRecord {
        offset: u64,
        #[source]
        source: DecodeError,
    },
}

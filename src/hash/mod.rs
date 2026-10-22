use std::io;

use thiserror::Error;

use crate::backend::record::DecodeError;

pub mod simple;

pub trait HashIndex {
    fn get(&self, key: String) -> Option<String>;
    fn set(&mut self, key: String, value: String) -> Result<(), HashIndexError>;
}

#[derive(Error, Debug)]
pub enum HashIndexError {
    #[error("Error creating hash index: {0}")]
    IOError(#[from] io::Error),
    #[error("Corrupt record at offset {offset}: {source}")]
    CorruptRecord {
        offset: u64,
        #[source]
        source: DecodeError,
    },
}

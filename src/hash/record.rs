//! Payload format for the records a hash index writes: `[u32 key length][key][value]`.
//!
//! Storing the key next to its value is what lets an index be rebuilt by replaying the store.

use std::{io, str::Utf8Error};

use thiserror::Error;

const KEY_LENGTH_PREFIX_LEN: usize = 4;

/// A decoded record, borrowing from the payload it was decoded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyValue<'a> {
    pub key: &'a str,
    pub value: &'a [u8],
}

#[derive(Error, Debug, PartialEq, Eq)]
pub enum DecodeError {
    #[error("payload of {payload_len} bytes is too short for the key length prefix")]
    MissingKeyLength { payload_len: usize },
    #[error("key length {key_len} exceeds the {remaining} bytes left in the payload")]
    KeyOutOfBounds { key_len: u32, remaining: usize },
    #[error("key is not valid UTF-8: {0}")]
    InvalidKey(#[from] Utf8Error),
}

pub(crate) fn encode(key: &str, value: &[u8]) -> io::Result<Vec<u8>> {
    let key_len = u32::try_from(key.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "key too large"))?;

    let mut payload = Vec::with_capacity(
        KEY_LENGTH_PREFIX_LEN
            .saturating_add(key.len())
            .saturating_add(value.len()),
    );
    payload.extend_from_slice(&key_len.to_le_bytes());
    payload.extend_from_slice(key.as_bytes());
    payload.extend_from_slice(value);
    Ok(payload)
}

pub(crate) fn decode(payload: &[u8]) -> Result<KeyValue<'_>, DecodeError> {
    let (key_len, rest) = payload.split_first_chunk::<KEY_LENGTH_PREFIX_LEN>().ok_or(
        DecodeError::MissingKeyLength {
            payload_len: payload.len(),
        },
    )?;
    let key_len = u32::from_le_bytes(*key_len);

    let (key, value) = usize::try_from(key_len)
        .ok()
        .and_then(|len| rest.split_at_checked(len))
        .ok_or(DecodeError::KeyOutOfBounds {
            key_len,
            remaining: rest.len(),
        })?;

    Ok(KeyValue {
        key: std::str::from_utf8(key)?,
        value,
    })
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::empty_key_and_value("", b"")]
    #[case::empty_value("key", b"")]
    #[case::empty_key("", b"value")]
    #[case::multibyte_key("clé", b"valeur")]
    #[case::binary_value("k", &[0, 255, 4, 0, 0, 0])]
    fn decode_returns_what_encode_wrote(#[case] key: &str, #[case] value: &[u8]) {
        let payload = encode(key, value).unwrap();

        assert_eq!(decode(&payload).unwrap(), KeyValue { key, value });
    }

    #[rstest]
    #[case::empty(&[], DecodeError::MissingKeyLength { payload_len: 0 })]
    #[case::partial_prefix(&[1, 0, 0], DecodeError::MissingKeyLength { payload_len: 3 })]
    #[case::key_past_end(&[5, 0, 0, 0, b'a'], DecodeError::KeyOutOfBounds { key_len: 5, remaining: 1 })]
    fn decode_rejects_payloads_that_cannot_hold_their_key(
        #[case] payload: &[u8],
        #[case] expected: DecodeError,
    ) {
        assert_eq!(decode(payload).unwrap_err(), expected);
    }

    #[test]
    fn decode_rejects_keys_that_are_not_utf8() {
        let payload = [1, 0, 0, 0, 0xFF];

        assert!(matches!(decode(&payload), Err(DecodeError::InvalidKey(_))));
    }
}

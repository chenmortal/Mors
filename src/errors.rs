use std::{io::Error, path::PathBuf};

use anyhow::anyhow;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum DBError {
    #[error("Invalid ValueLogFileSize, must be in range [1MB,2GB)")]
    ValuelogSize,
    #[error("Key not found")]
    KeyNotFound,
    #[error("Txn is too big to fit into one request")]
    TxnTooBig,
    #[error("Transaction Conflict. Please retry")]
    Conflict,
    #[error("No sets or deletes are allowed in a read-only transaction")]
    ReadOnlyTxn,
    #[error("This transaction has been discarded. Create a new one")]
    DiscardedTxn,
    #[error("Key cannot be empty")]
    EmptyKey,
    #[error("Key is using a reserved !badger! prefix")]
    InvalidKey,
    #[error("Key is using the banned prefix")]
    BannedKey,
    #[error("Value log GC can't run because threshold is set to zero")]
    ThresholdZero,
    #[error("Encryption key's length should be either 16 or 32 bytes")]
    InvalidEncryptionKey,
    #[error("Encryption key mismatch")]
    EncryptionKeyMismatch,
    #[error("Invalid datakey id")]
    InvalidDataKeyID,
    #[error("DB Closed")]
    DBClosed,
    #[error("Log truncate required to run DB. This might result in data loss ; end offset: {0} < size: {1} ")]
    TruncateNeeded(usize, usize),
    #[error("Writes are blocked, possibly due to DropAll or Close")]
    BlockedWrites, // ErrInvalidEncryptionKey is returned if length of encryption keys is invalid.
}
pub(crate) fn err_file(err: Error, path: &PathBuf, msg: &str) -> anyhow::Error {
    anyhow!("{}. Path={:?}. Error={}", msg, path, err)
}
// #[derive(Debug,Error)]
// pub enum FileSysErr {
//     #[error("Error opening {file_path} : {source}")]
//     CannotOpen{
//         source:std::io::Error,
//         file_path:PathBuf
//     },
//     #[error("Error get absolute path")]
// }

// impl From<std::io::Error> for FileSysErr{
//     fn from(value: std::io::Error) -> Self {
//         todo!()
//     }
// }
// impl From for FileSysErr {

// }

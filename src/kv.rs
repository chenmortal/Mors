use crate::txn::TxnTs;
use bincode::{DefaultOptions, Options};
use bytes::{Buf, BufMut};
use serde::{Deserialize, Serialize};
use std::ops::Deref;
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyTs {
    key: Vec<u8>,
    txn_ts: TxnTs,
}
impl PartialOrd for KeyTs {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.key.partial_cmp(&other.key) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        other.txn_ts.partial_cmp(&self.txn_ts)
    }
}
impl Ord for KeyTs {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.key.cmp(&other.key) {
            core::cmp::Ordering::Equal => {}
            ord => return ord,
        }
        other.txn_ts.cmp(&self.txn_ts)
    }
}
impl KeyTs {
    pub(crate) fn new(key: &[u8], ts: TxnTs) -> Self {
        Self {
            key: key.to_vec(),
            txn_ts: ts,
        }
    }
    pub(crate) fn get_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.key.len() + 8);
        v.put_slice(&self.key);
        v.put_u64(self.txn_ts.into());
        v
    }
    pub(crate) fn key(&self) -> &[u8] {
        &self.key
    }
    pub(crate) fn txn_ts(&self) -> &TxnTs {
        &self.txn_ts
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct KeyTsBorrow<'a>(&'a [u8]);
impl Deref for KeyTsBorrow<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl PartialOrd for KeyTsBorrow<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let self_split = self.len() - 8;
        let other_split = other.len() - 8;
        match self[..self_split].partial_cmp(&other[..other_split]) {
            Some(std::cmp::Ordering::Equal) => {}
            ord => {
                return ord;
            }
        }
        other[other_split..].partial_cmp(&self[self_split..])
    }
}
impl Ord for KeyTsBorrow<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let self_split = self.len() - 8;
        let other_split = other.len() - 8;
        match self[..self_split].cmp(&other[..other_split]) {
            std::cmp::Ordering::Equal => {}
            ord => {
                return ord;
            }
        }
        other[other_split..].cmp(&self[self_split..])
    }
}

impl From<&[u8]> for KeyTs {
    fn from(value: &[u8]) -> Self {
        let len = value.len();
        if len <= 8 {
            Self {
                key: value.to_vec(),
                txn_ts: 0.into(),
            }
        } else {
            let mut p = &value[len - 8..];
            Self {
                key: value[..len - 8].to_vec(),
                txn_ts: p.get_u64().into(),
            }
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub(crate) struct ValueInner {
    meta: u8,
    user_meta: u8,
    expires_at: u64,
    value: Vec<u8>,
}
#[derive(Debug, Default)]
pub(crate) struct ValueStruct {
    inner: ValueInner,
    version: TxnTs,
}
impl ValueStruct {
    fn encode(&self) -> Result<Vec<u8>, Box<bincode::ErrorKind>> {
        DefaultOptions::new()
            .with_varint_encoding()
            .serialize(&self.inner)
    }
    pub(crate) fn value(&self) -> &Vec<u8> {
        &self.inner.value
    }
    pub(crate) fn meta(&self) -> u8 {
        self.inner.meta
    }
    pub(crate) fn user_meta(&self)->u8{
        self.inner.user_meta
    }
    pub(crate) fn expires_at(&self) -> u64 {
        self.inner.expires_at
    }
    pub(crate) fn version(&self)->TxnTs{
        self.version
    }
}
#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use crate::kv::KeyTsBorrow;

    use super::KeyTs;

    #[test]
    fn test_bytes_from() {
        use crate::kv::KeyTs;
        let key_ts = KeyTs::new(b"a", 1.into());
        let bytes = key_ts.get_bytes();
        assert_eq!(KeyTs::from(bytes.as_ref()), key_ts);
    }
    #[test]
    fn test_ord() {
        let a = KeyTs::new(b"a", 1.into());
        let b = KeyTs::new(b"b", 0.into());
        let c = KeyTs::new(b"a", 2.into());
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(a.cmp(&c), Ordering::Greater);
        let a = &a.get_bytes();
        let b = &b.get_bytes();
        let c = &c.get_bytes();
        let a = KeyTsBorrow(a);
        let b = KeyTsBorrow(b);
        let c = KeyTsBorrow(c);
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(a.cmp(&c), Ordering::Greater);
    }
}
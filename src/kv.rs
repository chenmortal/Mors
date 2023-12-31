use bytes::{Buf, BufMut, Bytes, BytesMut};
use integer_encoding::VarInt;
use std::{
    cmp::Ordering,
    fmt::Display,
    mem,
    ops::{Add, AddAssign, Deref, Sub},
    time::{Duration, SystemTime, SystemTimeError},
};

use crate::vlog::header::VlogEntryHeader;

#[derive(Debug, Default, Clone)]
pub struct Entry {
    key_ts: KeyTs,
    value_meta: ValueMeta,
    offset: usize,
    header_len: usize,
    value_threshold: usize,
}

impl Entry {
    pub fn new(key: Bytes, value: Bytes) -> Self {
        let key_ts = KeyTs::new(key, TxnTs::default());
        let value_meta = ValueMeta {
            value,
            expires_at: PhyTs::default(),
            user_meta: 0,
            meta: Meta::default(),
        };
        Self {
            key_ts,
            offset: 0,
            header_len: 0,
            value_meta,
            value_threshold: 0,
        }
    }

    pub fn key(&self) -> &Bytes {
        &self.key_ts.key()
    }

    pub fn set_key<B: Into<Bytes>>(&mut self, key: B) {
        self.key_ts.set_key(key.into());
    }

    pub fn set_value<B: Into<Bytes>>(&mut self, value: B) {
        self.value_meta.value = value.into();
    }

    pub fn value(&self) -> &Bytes {
        &self.value_meta.value
    }

    pub fn set_expires_at(&mut self, expires_at: u64) {
        self.value_meta.expires_at = expires_at.into();
    }

    pub fn expires_at(&self) -> PhyTs {
        self.value_meta.expires_at
    }

    pub fn version(&self) -> TxnTs {
        self.key_ts.txn_ts()
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn set_user_meta(&mut self, user_meta: u8) {
        self.value_meta.user_meta = user_meta;
    }

    pub fn user_meta(&self) -> u8 {
        self.value_meta.user_meta
    }

    pub fn meta(&self) -> Meta {
        self.value_meta.meta
    }
}
impl Entry {
    pub(crate) fn header_len(&self) -> usize {
        self.header_len
    }

    pub(crate) fn set_header_len(&mut self, header_len: usize) {
        self.header_len = header_len;
    }
    pub(crate) fn estimate_size(&self, threshold: usize) -> usize {
        if self.value().len() < threshold {
            self.key().len() + self.value().len() + 2
        } else {
            self.key().len() + 12 + 2
        }
    }
    pub(crate) fn key_ts(&self) -> &KeyTs {
        &self.key_ts
    }
    pub(crate) fn set_meta(&mut self, meta: Meta) {
        self.value_meta.meta = meta;
    }
    pub(crate) fn value_meta(&self) -> &ValueMeta {
        &self.value_meta
    }
    pub(crate) fn meta_mut(&mut self) -> &mut Meta {
        &mut self.value_meta.meta
    }

    pub(crate) fn set_offset(&mut self, offset: usize) {
        self.offset = offset;
    }
    pub(crate) fn set_version(&mut self, version: TxnTs) {
        self.key_ts.set_txn_ts(version)
    }
    #[inline]
    pub(crate) fn new_ts(
        key_ts: &[u8],
        value: &[u8],
        header: &VlogEntryHeader,
        offset: usize,
        header_len: usize,
    ) -> Self {
        let k: KeyTs = key_ts.into();
        let value_meta = ValueMeta {
            value: value.to_vec().into(),
            expires_at: PhyTs::default(),
            user_meta: 0,
            meta: Meta::default(),
        };
        Self {
            key_ts: k,
            offset,
            header_len,
            value_meta,
            value_threshold: 0,
        }
    }
    #[inline]
    pub(crate) fn is_deleted(&self) -> bool {
        self.meta().contains(Meta::DELETE)
    }

    pub(crate) fn is_expired(&self) -> bool {
        if self.expires_at() == PhyTs::default() {
            return false;
        }
        self.expires_at() <= PhyTs::now().unwrap()
    }
    pub(crate) fn try_set_value_threshold(&mut self, threshold: usize) {
        if self.value_threshold == 0 {
            self.value_threshold = threshold;
        }
    }

    pub(crate) fn value_threshold(&self) -> usize {
        self.value_threshold
    }
}
///this means TransactionTimestamp
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxnTs(u64);
impl TxnTs {
    #[inline(always)]
    pub(crate) fn to_u64(&self) -> u64 {
        self.0
    }
}
impl Add<u64> for TxnTs {
    type Output = Self;

    fn add(self, rhs: u64) -> Self::Output {
        (self.0 + rhs).into()
    }
}
impl Sub<u64> for TxnTs {
    type Output = Self;

    fn sub(self, rhs: u64) -> Self::Output {
        (self.0 - rhs).into()
    }
}
impl AddAssign<u64> for TxnTs {
    fn add_assign(&mut self, rhs: u64) {
        self.0 += rhs;
    }
}
impl Display for TxnTs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("TxnTs:{}", self.0))
    }
}
impl From<u64> for TxnTs {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub struct PhyTs(u64);
impl From<u64> for PhyTs {
    fn from(value: u64) -> Self {
        Self(value)
    }
}
impl Into<u64> for PhyTs {
    fn into(self) -> u64 {
        self.0
    }
}
impl Into<SystemTime> for PhyTs {
    fn into(self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(self.0))
            .unwrap()
    }
}
impl PhyTs {
    pub(crate) fn to_u64(&self) -> u64 {
        self.0
    }
    pub(crate) fn now() -> Result<Self, SystemTimeError> {
        Ok(SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
            .into())
    }
}
#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub(crate) struct KeyTs {
    key: Bytes,
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
impl PartialEq<KeyTsBorrow<'_>> for KeyTs {
    fn eq(&self, other: &KeyTsBorrow<'_>) -> bool {
        self.key == other.key() && self.txn_ts() == other.txn_ts()
    }
}
impl PartialOrd<KeyTsBorrow<'_>> for KeyTs {
    fn partial_cmp(&self, other: &KeyTsBorrow<'_>) -> Option<std::cmp::Ordering> {
        match self.key().partial_cmp(other.key()) {
            Some(Ordering::Equal) => {}
            ord => {
                return ord;
            }
        };
        other.txn_ts().partial_cmp(&self.txn_ts())
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
impl From<&[u8]> for KeyTs {
    fn from(value: &[u8]) -> Self {
        let len = value.len();
        if len <= 8 {
            Self {
                key: value.to_vec().into(),
                txn_ts: 0.into(),
            }
        } else {
            let mut p = &value[len - 8..];
            Self {
                key: value[..len - 8].to_vec().into(),
                txn_ts: p.get_u64().into(),
            }
        }
    }
}
impl From<KeyTsBorrow<'_>> for KeyTs {
    fn from(value: KeyTsBorrow<'_>) -> Self {
        Self {
            key: value.key().to_vec().into(),
            txn_ts: value.txn_ts(),
        }
    }
}
impl KeyTs {
    pub(crate) fn new(key: Bytes, txn_ts: TxnTs) -> Self {
        Self { key, txn_ts }
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(self.key.len() + 8);
        v.put_slice(&self.key);
        v.put_u64(self.txn_ts.to_u64());
        v
    }

    pub(crate) fn key(&self) -> &Bytes {
        &self.key
    }

    pub(crate) fn txn_ts(&self) -> TxnTs {
        self.txn_ts
    }

    pub(crate) fn set_key(&mut self, key: Bytes) {
        self.key = key;
    }

    pub(crate) fn set_txn_ts(&mut self, txn_ts: TxnTs) {
        self.txn_ts = txn_ts;
    }

    pub(crate) fn len(&self) -> usize {
        self.key.len() + std::mem::size_of::<u64>()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) struct KeyTsBorrow<'a>(&'a [u8]);
impl<'a> KeyTsBorrow<'a> {
    pub(crate) fn key(&self) -> &[u8] {
        if self.len() >= 8 {
            &self[..self.len() - 8]
        } else {
            &self[..]
        }
    }
    pub(crate) fn txn_ts(&self) -> TxnTs {
        if self.len() >= 8 {
            let mut p = &self[self.len() - 8..];
            p.get_u64().into()
        } else {
            TxnTs::default()
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.key().is_empty()
    }
}

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
        KeyTsBorrow::cmp(&self, &other)
    }
}
impl PartialEq<KeyTs> for KeyTsBorrow<'_> {
    fn eq(&self, other: &KeyTs) -> bool {
        self.key() == other.key() && self.txn_ts() == other.txn_ts()
    }
}
impl PartialOrd<KeyTs> for KeyTsBorrow<'_> {
    fn partial_cmp(&self, other: &KeyTs) -> Option<Ordering> {
        match self.key().partial_cmp(other.key()) {
            Some(Ordering::Equal) => {}
            ord => {
                return ord;
            }
        }
        other.txn_ts().partial_cmp(&self.txn_ts())
    }
}
impl KeyTsBorrow<'_> {
    pub(crate) fn cmp(left: &[u8], right: &[u8]) -> std::cmp::Ordering {
        if left.len() > 8 && right.len() > 8 {
            let left_split = left.len() - 8;
            let right_split = right.len() - 8;
            match left[..left_split].cmp(&right[..right_split]) {
                std::cmp::Ordering::Equal => {}
                ord => {
                    return ord;
                }
            }
            right[right_split..].cmp(&left[left_split..])
        } else {
            left.cmp(right)
        }
    }
    pub(crate) fn equal_key(left: &[u8], right: &[u8]) -> bool {
        if left.len() > 8 && right.len() > 8 {
            let left_split = left.len() - 8;
            let right_split = right.len() - 8;
            left[..left_split] == right[..right_split]
        } else {
            left == right
        }
    }
}
impl<'a> From<&'a [u8]> for KeyTsBorrow<'a> {
    fn from(value: &'a [u8]) -> Self {
        Self(value)
    }
}
impl<'a> AsRef<[u8]> for KeyTsBorrow<'a> {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
impl<'a> Into<&'a [u8]> for KeyTsBorrow<'a> {
    fn into(self) -> &'a [u8] {
        &self.0
    }
}
#[derive(Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Meta(u8);
bitflags::bitflags! {
    impl Meta: u8 {
        const DELETE = 1<<0;
        const VALUE_POINTER = 1 << 1;
        const DISCARD_EARLIER_VERSIONS = 1 << 2;
        const MERGE_ENTRY=1<<3;
        const TXN=1<<6;
        const FIN_TXN=1<<7;
    }
}
impl std::fmt::Debug for Meta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        bitflags::parser::to_writer(self, f)
    }
}
impl std::fmt::Display for Meta {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        bitflags::parser::to_writer(self, f)
    }
}

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub(crate) struct ValueMeta {
    value: Bytes,
    expires_at: PhyTs,
    user_meta: u8,
    meta: Meta,
}
lazy_static! {
    static ref VALUEMETA_MIN_SERIALIZED_SIZE: usize = ValueMeta::default().serialized_size();
}
impl ValueMeta {
    pub(crate) fn serialized_size(&self) -> usize {
        2 + self.expires_at.0.required_space() + self.value.len()
    }

    pub(crate) fn serialize(&self) -> Vec<u8> {
        let mut v = vec![0u8; self.serialized_size()];
        v[0] = self.user_meta;
        v[1] = self.meta().0;
        let p = self.expires_at.0.encode_var(&mut v[2..]);
        v[2 + p..].copy_from_slice(self.value());
        v
    }

    pub(crate) fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < VALUEMETA_MIN_SERIALIZED_SIZE.to_owned() {
            return None;
        }
        if let Some((expires_at, size)) = u64::decode_var(&data[2..]) {
            return Self {
                value: data[2 + size..].to_vec().into(),
                expires_at: expires_at.into(),
                user_meta: data[0],
                meta: Meta(data[1]),
            }
            .into();
        }
        None
    }

    pub(crate) fn meta(&self) -> Meta {
        self.meta
    }

    pub(crate) fn value(&self) -> &Bytes {
        &self.value
    }

    pub(crate) fn set_value(&mut self, value: Bytes) {
        self.value = value;
    }
    pub(crate) fn is_deleted_or_expired(&self) -> bool {
        if self.meta.contains(Meta::DELETE) {
            return true;
        };
        if self.expires_at == PhyTs::default() {
            return false;
        }
        self.expires_at <= PhyTs::now().unwrap()
    }
}

#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ValuePointer {
    fid: u32,
    len: u32,
    offset: u32,
}
impl ValuePointer {
    const SIZE: usize = mem::size_of::<ValuePointer>();
    pub(crate) fn new(fid: u32, len: usize, offset: usize) -> Self {
        Self {
            fid: fid,
            len: len as u32,
            offset: offset as u32,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == ValuePointer::default()
    }

    pub(crate) fn serialize(&self) -> Bytes {
        let mut res = BytesMut::with_capacity(Self::SIZE);
        res.put_u32(self.fid);
        res.put_u32(self.len);
        res.put_u32(self.offset);
        res.freeze()
    }

    pub(crate) fn deserialize(bytes: &[u8]) -> Self {
        let mut p: &[u8] = bytes.as_ref();

        Self {
            fid: p.get_u32(),
            len: p.get_u32(),
            offset: p.get_u32(),
        }
    }

    pub(crate) fn len(&self) -> u32 {
        self.len
    }

    pub(crate) fn fid(&self) -> u32 {
        self.fid
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use bytes::Bytes;

    use crate::kv::{KeyTsBorrow, Meta, ValueMeta};

    use super::KeyTs;

    #[test]
    fn test_bytes_from() {
        use crate::kv::KeyTs;
        let key_ts = KeyTs::new("a".into(), 1.into());
        let bytes = key_ts.serialize();
        assert_eq!(KeyTs::from(bytes.as_ref()), key_ts);
    }
    #[test]
    fn test_ord() {
        let a = KeyTs::new("a".into(), 1.into());
        let b = KeyTs::new("b".into(), 0.into());
        let c = KeyTs::new("a".into(), 2.into());
        let default = KeyTs::default();
        assert!(a > default);
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(a.cmp(&c), Ordering::Greater);
        let a = &a.serialize();
        let b = &b.serialize();
        let c = &c.serialize();
        let a = KeyTsBorrow(a);
        let b = KeyTsBorrow(b);
        let c = KeyTsBorrow(c);
        assert_eq!(a.cmp(&b), Ordering::Less);
        assert_eq!(a.cmp(&c), Ordering::Greater);
    }
    #[test]
    fn test_partial_ord() {
        let p = b"abc";
        let m = KeyTsBorrow(p);
        let mut n = KeyTs::default();
        n.set_key(Bytes::from("abc"));
        n.set_txn_ts(1.into());

        dbg!(n.partial_cmp(&m));
        let a = vec![];
        let b = KeyTsBorrow(&a);
        dbg!(b.is_empty());
        let c = KeyTs::default().serialize();
        assert!(KeyTsBorrow(&c).is_empty());
    }
    #[test]
    fn test_serialize() {
        let mut v = ValueMeta::default();
        v.value = String::from("abc").as_bytes().to_vec().into();
        v.expires_at = 123456789.into();
        v.meta = Meta(1);
        assert_eq!(v.serialized_size(), 9);
        assert_eq!(v, ValueMeta::deserialize(&v.serialize()).unwrap());
    }
    #[test]
    fn test_empty() {
        let mut v = ValueMeta::default();
        v.value = Bytes::from("");
        dbg!(v.serialized_size());

        let k = KeyTs::new(Bytes::default(), 0.into());
        dbg!(k.key().len());
        // let mut v = ValueMeta::default().serialized_size();

        // dbg!(v);
    }
    #[test]
    fn test_meta() {
        assert!(Meta(0).is_empty());
        assert!(!Meta(1).is_empty())
    }
    #[test]
    fn test_equal_key() {
        let a = KeyTs::new(b"a".as_ref().into(), 1.into()).serialize();
        let a_clone = KeyTs::new(b"a".as_ref().into(), 1.into()).serialize();
        assert!(KeyTsBorrow::equal_key(&a, &a_clone));

        let b = KeyTs::new(b"ab".as_ref().into(), 1.into()).serialize();
        assert!(!KeyTsBorrow::equal_key(&a, &b));
    }
}

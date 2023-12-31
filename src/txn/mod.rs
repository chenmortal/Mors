mod item;
pub(crate) mod oracle;
mod water_mark;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;

use ahash::RandomState;
use anyhow::bail;
use bytes::Bytes;
use rand::{thread_rng, Rng};

use crate::kv::Entry;
use crate::kv::Meta;
use crate::{db::DB, errors::DBError, kv::KeyTs};

use self::item::PrefetchStatus;
use self::item::{Item, ItemInner};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, AtomicI32},
};

use parking_lot::Mutex;
use tokio::sync::oneshot::Receiver;

use crate::kv::TxnTs;

/// Prefix for internal keys used by badger.
const BADGER_PREFIX: &[u8] = b"!badger!";
/// For indicating end of entries in txn.
const TXN_KEY: &[u8] = b"!badger!txn";
/// For storing the banned namespaces.
const BANNED_NAMESPACES_KEY: &[u8] = b"!badger!banned";

lazy_static! {
    pub(crate) static ref HASH: RandomState = ahash::RandomState::with_seed(thread_rng().gen());
}

//Transaction
pub trait TxnUpdate {
    async fn update(self, txn: &mut Txn) -> anyhow::Result<()>;
}
impl DB {
    pub async fn update<F: TxnUpdate>(&self, f: F) -> anyhow::Result<()> {
        #[inline]
        async fn run<F: TxnUpdate>(txn: &mut Txn, f: F) -> anyhow::Result<()> {
            f.update(txn).await?;
            txn.commit().await
        }

        if self.is_closed() {
            bail!(DBError::DBClosed);
        }

        if self.oracle.config().managed_txns {
            panic!("Update can only be used with managed_txns=false");
        }
        let mut txn = Txn::new(self.clone(), true, false).await?;
        let result = run(&mut txn, f).await;
        txn.discard().await?;
        result
    }

    pub async fn get_update_txn(&self) -> anyhow::Result<Txn> {
        if self.is_closed() {
            bail!(DBError::DBClosed);
        }
        if self.oracle.config().managed_txns {
            panic!("Update can only be used with managed_txns=false");
        }
        let txn = Txn::new(self.clone(), true, false).await?;
        Ok(txn)
    }
}
impl Txn {
    pub async fn get<B: Into<Bytes>>(&self, key: B) -> anyhow::Result<Item> {
        let key: Bytes = key.into();
        if key.len() == 0 {
            bail!(DBError::EmptyKey);
        } else if self.discarded() {
            bail!(DBError::DiscardedTxn);
        }
        self.db().is_banned(&key).await?;
        let mut item = ItemInner::default();
        if self.update() {
            if let Some(pending_writes) = self.pending_writes() {
                if let Some(entry) = pending_writes.get(key.as_ref()) {
                    if entry.key().as_ref() == key {
                        if entry.value_meta().is_deleted_or_expired() {
                            bail!(DBError::KeyNotFound);
                        }
                        let mut key_ts = entry.key_ts().clone();
                        key_ts.set_txn_ts(self.read_ts);
                        item.set_key_ts(key_ts);
                        item.set_value_meta(entry.value_meta().clone());
                        item.set_status(PrefetchStatus::Prefetched);
                        return Ok(item.into());
                    }
                };
            }
            let hash = HASH.hash_one(key.clone());
            let mut reads_m = self.read_key_hash().lock();
            reads_m.push(hash);
            drop(reads_m);
        }

        let mut seek = KeyTs::new(key.clone(), self.read_ts);
        let (txn_ts, value_meta) = match self.db().get(&seek).await? {
            Some((txn_ts, value_meta)) => {
                if value_meta.value().is_empty() && value_meta.meta().is_empty() {
                    bail!(DBError::KeyNotFound)
                }
                if value_meta.is_deleted_or_expired() {
                    bail!(DBError::KeyNotFound)
                }
                (txn_ts, value_meta)
            }
            None => bail!(DBError::KeyNotFound),
        };
        seek.set_txn_ts(txn_ts);
        item.set_key_ts(seek);
        item.set_value_meta(value_meta);
        Ok(item.into())
    }

    pub async fn set<B: Into<Bytes>>(&mut self, key: B, value: B) -> anyhow::Result<()> {
        self.set_entry(Entry::new(key.into(), value.into())).await
    }

    pub async fn delete<B: Into<Bytes>>(&mut self, key: B) -> anyhow::Result<()> {
        let mut e = Entry::default();
        e.set_key(key);
        e.set_meta(Meta::DELETE);
        self.set_entry(e).await
    }

    pub async fn set_entry(&mut self, e: Entry) -> anyhow::Result<()> {
        self.modify(e.into()).await
    }

    pub async fn commit(&mut self) -> anyhow::Result<()> {
        match self.pending_writes().as_ref() {
            Some(s) => {
                if s.len() == 0 {
                    return Ok(());
                }
            }
            None => {
                return Ok(());
            }
        };
        self.commit_pre_check()?;
        let (commit_ts, recv) = self.commit_and_send().await?;
        let result = recv.await;
        self.db().oracle.done_commit(commit_ts).await?;
        result??;
        Ok(())
    }

    pub async fn discard(mut self) -> anyhow::Result<()> {
        if self.discarded() {
            return Ok(());
        }
        if self.num_iters().load(Ordering::Acquire) > 0 {
            panic!("Unclosed iterator at time of Txn.discard.")
        }
        self.set_discarded(true);

        if !self.txn_config.managed_txns {
            self.db().oracle.done_read(&self).await?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy)]
pub struct TxnConfig {
    read_only: bool,
    // DetectConflicts determines whether the transactions would be checked for
    // conflicts. The transactions can be processed at a higher rate when
    // conflict detection is disabled.
    detect_conflicts: bool,
    // Transaction start and commit timestamps are managed by end-user.
    // This is only useful for databases built on top of Badger (like Dgraph).
    // Not recommended for most users.
    managed_txns: bool,
}
impl Default for TxnConfig {
    fn default() -> Self {
        Self {
            read_only: false,
            detect_conflicts: true,
            managed_txns: false,
        }
    }
}
pub struct Txn {
    read_ts: TxnTs,
    commit_ts: TxnTs,
    size: usize,
    count: usize,
    txn_config: TxnConfig,
    db: DB,
    conflict_keys: Option<HashSet<u64>>,
    read_key_hash: Mutex<Vec<u64>>,
    pending_writes: Option<HashMap<Bytes, Entry>>, // Vec<u8> -> String
    duplicate_writes: Vec<Entry>,
    num_iters: AtomicI32,
    discarded: bool,
    done_read: AtomicBool,
    update: bool,
}
impl Txn {
    pub(super) async fn new(db: DB, mut update: bool, is_managed: bool) -> anyhow::Result<Self> {
        let txn_config = db.oracle.config();
        if txn_config.read_only && update {
            update = false;
        }

        let s = Self {
            read_ts: if !is_managed {
                db.oracle.get_latest_read_ts().await?
            } else {
                TxnTs::default()
            },
            commit_ts: TxnTs::default(),
            size: TXN_KEY.len() + 10,
            count: 1, // One extra entry for BitFin.

            conflict_keys: if update && txn_config.detect_conflicts {
                Some(HashSet::new())
            } else {
                None
            },

            read_key_hash: Mutex::new(Vec::new()),
            pending_writes: if update { HashMap::new().into() } else { None },
            duplicate_writes: Default::default(),
            discarded: false,
            done_read: AtomicBool::new(false),
            db,
            update,
            num_iters: AtomicI32::new(0),
            txn_config,
        };
        Ok(s)
    }

    #[inline]
    pub(super) async fn modify(&mut self, mut e: Entry) -> anyhow::Result<()> {
        let exceeds_size = |prefix: &str, max: usize, key: &[u8]| {
            bail!(
                "{} with size {} exceeded {} limit. {}:\n{:?}",
                prefix,
                key.len(),
                max,
                prefix,
                if key.len() > 1024 { &key[..1024] } else { key }
            )
        };
        let threshold = self.db().opt.vlog_threshold.value_threshold();
        let max_batch_count = self.db().opt.max_batch_count();
        let max_batch_size = self.db().opt.max_batch_size();
        let vlog_file_size = self.db().opt.vlog.vlog_file_size();
        let mut check_size = |e: &mut Entry| {
            let count = self.count + 1;
            e.try_set_value_threshold(threshold);
            let size = self.size + e.estimate_size(threshold) + 10;
            if count >= max_batch_count || size >= max_batch_size {
                bail!(DBError::TxnTooBig)
            }
            self.count = count;
            self.size = size;
            Ok(())
        };

        const MAX_KEY_SIZE: usize = 65000;
        if !self.update {
            bail!(DBError::ReadOnlyTxn)
        }
        if self.discarded {
            bail!(DBError::DiscardedTxn)
        }
        if e.key().is_empty() {
            bail!(DBError::EmptyKey)
        }
        if e.key().starts_with(BADGER_PREFIX) {
            bail!(DBError::InvalidKey)
        }
        if e.key().len() > MAX_KEY_SIZE {
            exceeds_size("Key", MAX_KEY_SIZE, e.key())?;
        }
        if e.value().len() > vlog_file_size {
            exceeds_size("Value", vlog_file_size, e.value().as_ref())?
        }
        self.db.is_banned(&e.key()).await?;

        check_size(&mut e)?;

        if let Some(c) = self.conflict_keys.as_mut() {
            c.insert(HASH.hash_one(e.key()));
        }

        if let Some(p) = self.pending_writes.as_mut() {
            let new_version = e.version();
            if let Some(old) = p.insert(e.key().clone(), e) {
                if old.version() != new_version {
                    self.duplicate_writes.push(old);
                }
            };
        }
        Ok(())
    }

    pub(super) fn commit_pre_check(&self) -> anyhow::Result<()> {
        if self.discarded {
            bail!("Trying to commit a discarded txn")
        }
        let mut keep_togther = true;
        if let Some(s) = self.pending_writes.as_ref() {
            for (_, e) in s {
                if e.version() != TxnTs::default() {
                    keep_togther = false;
                    break;
                }
            }
        }
        if keep_togther && self.txn_config.managed_txns && self.commit_ts == TxnTs::default() {
            bail!("CommitTs cannot be zero. Please use commitat instead")
        }
        Ok(())
    }

    pub(super) async fn commit_and_send(
        &mut self,
    ) -> anyhow::Result<(TxnTs, Receiver<anyhow::Result<()>>)> {
        let oracle = &self.db.oracle;
        let _guard = oracle.send_write_req.lock();
        let commit_ts = oracle.get_latest_commit_ts(self).await?;
        let mut keep_together = true;
        let mut set_version = |e: &mut Entry| {
            if e.version() == TxnTs::default() {
                e.set_version(commit_ts)
            } else {
                keep_together = false;
            }
        };

        let mut pending_wirtes_len = 0;
        if let Some(p) = self.pending_writes.as_mut() {
            pending_wirtes_len = p.len();
            p.iter_mut().map(|x| x.1).for_each(&mut set_version);
        }
        let duplicate_writes_len = self.duplicate_writes.len();
        self.duplicate_writes.iter_mut().for_each(set_version);

        //read from pending_writes and duplicate_writes to Vec<>
        let mut entries = Vec::with_capacity(pending_wirtes_len + duplicate_writes_len + 1);
        let mut process_entry = |mut e: Entry| {
            if keep_together {
                e.meta_mut().insert(Meta::TXN)
            }
            entries.push(e);
        };

        if let Some(p) = self.pending_writes.as_mut() {
            p.drain()
                .take(pending_wirtes_len)
                .for_each(|(_, e)| process_entry(e));
        }
        self.duplicate_writes
            .drain(0..)
            .take(duplicate_writes_len)
            .for_each(|e| process_entry(e));

        if keep_together {
            debug_assert!(commit_ts != TxnTs::default());
            let mut entry = Entry::new(TXN_KEY.into(), commit_ts.to_u64().to_string().into());
            entry.set_version(commit_ts);
            entry.set_meta(Meta::FIN_TXN);
            entries.push(entry.into())
        }

        let receiver = match self
            .db
            .send_entires_to_write_channel(entries, self.size)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                oracle.done_commit(commit_ts).await?;
                bail!(e)
            }
        };

        Ok((commit_ts, receiver))
    }

    pub(super) fn discarded(&self) -> bool {
        self.discarded
    }

    pub(super) fn set_discarded(&mut self, discarded: bool) {
        self.discarded = discarded;
    }

    pub(super) fn db(&self) -> &DB {
        &self.db
    }

    pub(super) fn update(&self) -> bool {
        self.update
    }

    pub(super) fn pending_writes(&self) -> Option<&HashMap<Bytes, Entry>> {
        self.pending_writes.as_ref()
    }

    pub(super) fn num_iters(&self) -> &AtomicI32 {
        &self.num_iters
    }

    pub(super) fn done_read(&self) -> &AtomicBool {
        &self.done_read
    }

    pub(super) fn read_key_hash(&self) -> &Mutex<Vec<u64>> {
        &self.read_key_hash
    }

    pub(super) fn conflict_keys(&self) -> Option<&HashSet<u64>> {
        self.conflict_keys.as_ref()
    }
}
#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin};
    trait Update {
        async fn update(self, txn: &mut Txn) -> anyhow::Result<()>;
    }
    struct TxnManager;
    #[derive(Debug)]
    struct Txn;
    struct UpdateK;
    impl Update for UpdateK {
        async fn update(self, txn: &mut Txn) -> anyhow::Result<()> {
            println!("abc");
            Ok(())
        }
    }
    async fn update_data(txn: &mut Txn) -> anyhow::Result<()> {
        println!("abc");
        Ok(())
    }
    impl TxnManager {
        fn update_async<F: Update>(&self, f: F) -> anyhow::Result<()>
// where F:Update
        // where
        //     F: FnMut(&mut Txn) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>>,
        {
            let mut t = Txn;
            let _ = f.update(&mut t);
            Ok(())
        }
    }
    // fn ab<F>(f: Pin<Box<fn(&mut Txn) -> impl Future<Output = anyhow::Result<()>>>>) {}
    #[tokio::test]
    async fn test_txn() {
        let t = TxnManager;
        let k = Box::pin(update_data);
        // ab(k);
        // t.update_async(|txn| async {

        // });
    }
}

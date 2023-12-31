use std::collections::HashSet;
use std::ops::Deref;
use std::sync::Arc;
use std::{cmp::Ordering, io};

use anyhow::anyhow;
use tokio::sync::RwLock;

use crate::kv::{KeyTs, KeyTsBorrow};
use crate::util::SSTableId;
use crate::{table::Table, util::compare_key};

use super::levels::{Level, LEVEL0};
#[derive(Debug, Clone)]
pub(crate) struct LevelHandler(Arc<LevelHandlerInner>);
impl Deref for LevelHandler {
    type Target = Arc<LevelHandlerInner>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
#[derive(Debug)]
pub(crate) struct LevelHandlerInner {
    handler_tables: RwLock<LevelHandlerTables>,
    level: Level,
}

impl LevelHandlerInner {
    pub(crate) fn level(&self) -> Level {
        self.level
    }

    pub(crate) fn handler_tables(&self) -> &RwLock<LevelHandlerTables> {
        &self.handler_tables
    }
    pub(crate) async fn tables_len(&self) -> usize {
        let tables_r = self.read().await;
        let len = tables_r.tables.len();
        drop(tables_r);
        len
    }
}
impl Deref for LevelHandlerInner {
    type Target = RwLock<LevelHandlerTables>;

    fn deref(&self) -> &Self::Target {
        &self.handler_tables
    }
}
#[derive(Debug, Default)]
pub(crate) struct LevelHandlerTables {
    pub(crate) tables: Vec<Table>,
    pub(crate) total_size: usize,
    pub(crate) total_stale_size: u32,
}
impl LevelHandlerTables {
    pub(crate) fn init(&mut self, level: Level, tables: Vec<Table>) {
        self.tables = tables;
        let mut total_size = 0;
        let mut total_stale_size = 0;

        self.tables.iter().for_each(|t| {
            total_size += t.size();
            total_stale_size = t.stale_data_size();
        });

        self.total_size = total_size;
        self.total_stale_size = total_stale_size;

        if level == LEVEL0 {
            self.tables.sort_by(|a, b| a.table_id().cmp(&b.table_id()));
        } else {
            self.tables.sort_by(|a, b| a.smallest().cmp(b.smallest()));
        }
    }
}
impl LevelHandler {
    pub(crate) fn new(level: Level) -> Self {
        let inner = LevelHandlerInner {
            handler_tables: RwLock::new(LevelHandlerTables::default()),
            level,
        };
        Self(Arc::new(inner))
    }

    #[inline]
    pub(crate) async fn get_tables_len(&self) -> usize {
        let tables_r = self.read().await;
        let len = tables_r.tables.len();
        drop(tables_r);
        len
    }

    #[inline]
    pub(crate) async fn get_total_size(&self) -> usize {
        let inner_r = self.0.handler_tables.read().await;
        let total_size = inner_r.total_size;
        drop(inner_r);
        total_size
    }
    pub(crate) async fn init_tables(&self, tables: Vec<Table>) {
        let mut inner_w = self.0.handler_tables.write().await;
        inner_w.init(self.level(), tables);
        drop(inner_w);
    }
    pub(crate) async fn replace(&self, old: &[Table], new: &[Table]) {
        let mut inner_w = self.write().await;
        let to_del = old
            .iter()
            .map(|x| x.table_id())
            .collect::<HashSet<SSTableId>>();
        let mut new_tables = Vec::with_capacity(inner_w.tables.len() - to_del.len() + new.len());

        inner_w
            .tables
            .drain(..)
            .filter(|t| !to_del.contains(&t.table_id()))
            .for_each(|t| new_tables.push(t));
        new.iter().for_each(|t| new_tables.push(t.clone()));
        inner_w.init(self.level(), new_tables);
        drop(inner_w)
    }
    pub(crate) async fn delete(&self, del: &[Table]) {
        let mut inner_w = self.write().await;
        let to_del = del
            .iter()
            .map(|t| t.table_id())
            .collect::<HashSet<SSTableId>>();
        let mut new_tables = Vec::with_capacity(inner_w.tables.len() - to_del.len());
        let mut sub_total_size = 0;
        let mut sub_total_stale_size = 0;
        for table in inner_w.tables.drain(..) {
            if to_del.contains(&table.table_id()) {
                sub_total_size += table.size();
                sub_total_stale_size += table.stale_data_size();
            } else {
                new_tables.push(table);
            };
        }
        inner_w.tables = new_tables;
        inner_w.total_size -= sub_total_size;
        inner_w.total_stale_size -= sub_total_stale_size;
        drop(inner_w)
    }
    pub(crate) async fn validate(&self) -> anyhow::Result<()> {
        let inner_r = self.0.handler_tables.read().await;
        if self.level == LEVEL0 {
            return Ok(());
        }
        let num_tables = inner_r.tables.len();
        for j in 1..num_tables {
            let pre = &inner_r.tables[j - 1];
            let now = &inner_r.tables[j];
            let pre_biggest = pre.biggest();

            if pre_biggest.cmp(now.smallest()).is_ge() {
                let e = anyhow!(
                    "Inter: Biggest(j-1)[{:?}] 
{:?}
vs Smallest(j)[{:?}]: 
{:?}
: level={} j={} num_tables={}",
                    pre.table_id(),
                    pre_biggest,
                    now.table_id(),
                    now.smallest(),
                    self.0.level,
                    j,
                    num_tables
                );
                return Err(e);
            };

            let now_biggest = now.biggest();
            if now.smallest().cmp(now_biggest).is_gt() {
                let e = anyhow!(
                    "Intra:
{:?}
vs
{:?}
: level={} j={} num_tables={}",
                    now.smallest(),
                    now_biggest,
                    self.0.level,
                    j,
                    num_tables
                );
                return Err(e);
            };
        }
        Ok(())
    }
    pub(crate) async fn sync_mmap(&self) -> io::Result<()> {
        let mut err = None;
        let tables_r = self.0.handler_tables.read().await;
        for table in tables_r.tables.iter() {
            match table.sync_mmap() {
                Ok(_) => {}
                Err(e) => {
                    if err.is_none() {
                        err = e.into();
                    }
                }
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    // pub(crate) async fn overlapping_tables(&self, key_range: &KeyRange) -> (usize, usize) {
    //     let left = key_range.get_left();
    //     let right = key_range.get_right();
    //     if left.len() == 0 || right.len() == 0 {
    //         return (0, 0);
    //     };

    //     let self_r = self.0.handler_tables.read().await;
    //     let tables = &self_r.tables;
    //     let left_index = match binary_search_biggest(tables, left).await {
    //         Ok(index) => index,  // val[index].biggest == value
    //         Err(index) => index, // val[index].biggest > value
    //     };
    //     let right_index =
    //         match tables.binary_search_by(|table| table.smallest().partial_cmp(&KeyTsBorrow::from(right)).unwrap()) {
    //             Ok(index) => index + 1, //for i in left..right ; not include right so this need add 1;
    //             Err(index) => index,
    //         };
    //     drop(self_r);
    //     (left_index, right_index)
    // }
}

//fix from std binary_search_by
#[inline]
async fn binary_search_biggest(tables: &Vec<Table>, value: &[u8]) -> Result<usize, usize> {
    // INVARIANTS:
    // - 0 <= left <= left + size = right <= self.len()
    // - f returns Less for everything in self[..left]
    // - f returns Greater for everything in self[right..]

    // 0 1 2 3 4 5 t=2.5
    // l=0 r=6 s=6 mid=3 g
    // l=0 r=3 s=3 mid=1 l
    // l=2 r=3 s=1 mid=2 l
    // l=3 r=1
    // return 3
    let value = KeyTs::from(value);
    let mut size = tables.len();
    let mut left = 0;
    let mut right = size;
    while left < right {
        let mid = left + size / 2;

        // SAFETY: the while condition means `size` is strictly positive, so
        // `size/2 < size`. Thus `left + size/2 < left + size`, which
        // coupled with the `left + size <= self.len()` invariant means
        // we have `left + size/2 < self.len()`, and this is in-bounds.
        let mid_r = unsafe { tables.get_unchecked(mid) }.biggest();
        // let mid_slice: &[u8] = mid_r;
        // let cmp = compare_key(mid_slice, value);
        let cmp = mid_r.cmp(&value);
        // The reason why we use if/else control flow rather than match
        // is because match reorders comparison operations, which is perf sensitive.
        // This is x86 asm for u8: https://rust.godbolt.org/z/8Y8Pra.
        if cmp == Ordering::Less {
            left = mid + 1;
        } else if cmp == Ordering::Greater {
            right = mid;
        } else {
            return Ok(mid);
        }

        size = right - left;
    }
    Err(left)
}

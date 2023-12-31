use crate::util::{mmap::MmapFile, search};
use bytes::Buf;
use log::info;
use std::{fs::OpenOptions, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
const DISCARD_FILE_NAME: &str = "DISCARD";
const DISCARD_FILE_SIZE: usize = 1 << 20; //1MB
const DISCARD_MAX_SLOT: usize = DISCARD_FILE_SIZE / 16; //1MB file can store 65536 discard entries. Each entry is 16 bytes;
#[derive(Debug,Clone)]
pub(crate) struct DiscardStats(Arc<Mutex<DiscardStatsInner>>);
#[derive(Debug)]
struct DiscardStatsInner {
    mmap_f: MmapFile,
    next_empty_slot: usize,
}
impl DiscardStats {
    pub(crate) fn new(vlog_dir: &PathBuf) -> anyhow::Result<Self> {
        Ok(Self(Arc::new(Mutex::new(DiscardStatsInner::new(
            vlog_dir,
        )?))))
    }
    #[inline]
    pub(crate) async fn update(&self, fid: u64, discard: i64) -> anyhow::Result<u64> {
        let mut inner = self.0.lock().await;
        Ok(
            match search(inner.next_empty_slot, |slot| inner.get(slot * 16).cmp(&fid)) {
                Ok(index) => {
                    let offset = index * 16 + 8;
                    let mut cur_disc = inner.get(offset);
                    if discard == 0 {
                        cur_disc
                    } else if discard < 0 {
                        inner.set(offset, 0)?;
                        0
                    } else {
                        cur_disc += discard as u64;
                        inner.set(offset, cur_disc)?;
                        cur_disc
                    }
                }
                Err(_) => {
                    if discard <= 0 {
                        0
                    } else {
                        let index = inner.next_empty_slot;
                        inner.set(index * 16, fid)?;
                        inner.set(index * 16 + 8, discard as u64)?;

                        while inner.next_empty_slot >= inner.mmap_f.len() / 16 {
                            let len = inner.mmap_f.len();
                            inner.mmap_f.set_len(2 * len)?;
                        }
                        inner.sort();
                        discard as u64
                    }
                }
            },
        )
    }
}
impl DiscardStatsInner {
    fn new(vlog_dir: &PathBuf) -> anyhow::Result<Self> {
        let file_path = vlog_dir.join(DISCARD_FILE_NAME);
        let mut fp_open_opt = OpenOptions::new();
        fp_open_opt.read(true).write(true).create(true);

        let (mmap_f, is_new) = MmapFile::open(&file_path, fp_open_opt, DISCARD_FILE_SIZE)?;
        // let (mmap_f, is_new) = open_mmap_file(&file_path, fp_open_opt, DISCARD_FILE_SIZE)
        //     .map_err(|e| anyhow!("while openint file: {} for {} \n", DISCARD_FILE_NAME, e))?;
        let mut discard_stats = Self {
            mmap_f,
            next_empty_slot: 0,
        };

        if is_new {
            // discard_stats.zero_out();
        }
        for slot in 0..DISCARD_MAX_SLOT {
            if discard_stats.get(slot * 16) == 0 {
                discard_stats.next_empty_slot = slot;
                break;
            }
        }
        discard_stats.sort();
        info!(
            "Discard stats next_empty_slot:{} \n",
            discard_stats.next_empty_slot
        );
        Ok(discard_stats)
    }
    #[inline(always)]
    pub(crate) fn set(&mut self, offset: usize, val: u64) -> anyhow::Result<()> {
        let big_endian = val.to_be_bytes();
        self.mmap_f.write_slice(offset, big_endian.as_slice())?;
        Ok(())
    }
    #[inline(always)]
    pub(crate) fn get(&self, offset: usize) -> u64 {
        let mut p = &self.mmap_f.as_ref()[offset..offset + 8];
        p.get_u64()
    }

    #[inline]
    pub(crate) fn sort(&mut self) {
        let slice = &mut self.mmap_f.as_mut()[..self.next_empty_slot * 8 * 2];
        let chunks = unsafe { slice.as_chunks_unchecked_mut::<16>() };
        chunks.sort_unstable_by(|a, b| a.as_ref().get_u64().cmp(&b.as_ref().get_u64()));
    }
    #[inline]
    pub(crate) fn max_slot(&self) -> usize {
        self.mmap_f.len() / 16
    }
}

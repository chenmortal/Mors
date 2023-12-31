use std::{hash::Hasher, io::Write, mem, sync::atomic::Ordering};

use bytes::BufMut;

#[cfg(feature = "metrics")]
use crate::util::metrics::{add_num_bytes_vlog_written, add_num_writes_vlog};
use crate::{
    default::DEFAULT_PAGE_SIZE,
    kv::{Entry, Meta, TxnTs, ValuePointer},
    util::{log_file::LogFile, DBFileId},
    write::WriteReq,
};

use super::{header::VlogEntryHeader, ValueLog, MAX_HEADER_SIZE, MAX_VLOG_FILE_SIZE};
use anyhow::bail;
pub(crate) struct HashWriter<'a, T: Hasher> {
    writer: &'a mut Vec<u8>,
    hasher: T,
}

impl<T: Hasher> Write for HashWriter<'_, T> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.writer.put_slice(buf);
        self.hasher.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl ValueLog {
    pub(crate) async fn write(&self, reqs: &mut Vec<WriteReq>) -> anyhow::Result<()> {
        self.validate_write(reqs)?;

        let mut buf = Vec::with_capacity(DEFAULT_PAGE_SIZE.to_owned());
        #[cfg(feature = "metrics")]
        let sender = self.threshold.sender();
        let mut cur_logfile = self.get_latest_logfile().await?;

        for req in reqs.iter_mut() {
            let mut cur_logfile_w = cur_logfile.write().await;
            let entries_vptrs = req.entries_vptrs_mut();
            let mut value_sizes = Vec::with_capacity(entries_vptrs.len());
            let mut written = 0;
            #[cfg(feature = "metrics")]
            let mut bytes_written = 0;
            for (dec_entry, vptr) in entries_vptrs {
                value_sizes.push(dec_entry.value().len());
                dec_entry.try_set_value_threshold(self.threshold.value_threshold());
                if dec_entry.value().len() < dec_entry.value_threshold() {
                    if !vptr.is_empty() {
                        *vptr = ValuePointer::default();
                    }
                    continue;
                }
                let fid = cur_logfile_w.fid();
                let offset = self.writable_log_offset();

                let tmp_meta = dec_entry.meta();
                dec_entry.meta_mut().remove(Meta::TXN);
                dec_entry.meta_mut().remove(Meta::FIN_TXN);
                let len = cur_logfile_w.encode_entry(&mut buf, &dec_entry, offset);

                dec_entry.set_meta(tmp_meta);
                *vptr = ValuePointer::new(fid.into(), len, offset);

                if buf.len() != 0 {
                    let buf_len = buf.len();
                    let start_offset = self.writable_log_offset_fetch_add(buf_len);
                    let end_offset = start_offset + buf_len;
                    if end_offset >= cur_logfile_w.len() {
                        cur_logfile_w.truncate(end_offset)?;
                    };
                    cur_logfile_w.write_slice(start_offset, &buf)?;
                    // cur_logfile_w.mmap[start_offset..end_offset].copy_from_slice(&buf);
                }
                written += 1;
                #[cfg(feature = "metrics")]
                {
                    bytes_written += buf.len();
                }
            }
            #[cfg(feature = "metrics")]
            {
                add_num_writes_vlog(written);
                add_num_bytes_vlog_written(bytes_written);
                sender.send(value_sizes).await?;
            }

            self.num_entries_written
                .fetch_add(written, Ordering::SeqCst);

            let w_offset = self.writable_log_offset();
            if w_offset > self.config.vlog_file_size
                || self.num_entries_written.load(Ordering::SeqCst) > self.config.vlog_max_entries
            {
                if self.config.sync_writes {
                    cur_logfile_w.raw_sync()?;
                }
                cur_logfile_w.truncate(w_offset)?;
                let new = self.create_vlog_file().await?; //new logfile will be latest logfile
                drop(cur_logfile_w);
                cur_logfile = new;
            };
        }
        //wait for async closure trait
        let mut cur_logfile_w = cur_logfile.write().await;
        let w_offset = self.writable_log_offset();
        if w_offset > self.config.vlog_file_size
            || self.num_entries_written.load(Ordering::SeqCst) > self.config.vlog_max_entries
        {
            if self.config.sync_writes {
                cur_logfile_w.raw_sync()?;
            }
            cur_logfile_w.truncate(w_offset)?;
            let _ = self.create_vlog_file().await?; //new logfile will be latest logfile
        };
        Ok(())
    }
    fn validate_write(&self, reqs: &Vec<WriteReq>) -> anyhow::Result<()> {
        let mut vlog_offset = self.writable_log_offset();
        for req in reqs {
            let mut size = 0;
            req.entries_vptrs().iter().for_each(|(x, _)| {
                size += MAX_HEADER_SIZE
                    + x.key().len()
                    + mem::size_of::<TxnTs>()
                    + x.value().len()
                    + mem::size_of::<u32>()
            });
            let estimate = vlog_offset + size;
            if estimate > MAX_VLOG_FILE_SIZE {
                bail!(
                    "Request size offset {} is bigger than maximum offset {}",
                    estimate,
                    MAX_VLOG_FILE_SIZE
                )
            }

            if estimate >= self.config.vlog_file_size {
                vlog_offset = 0;
                continue;
            }
            vlog_offset = estimate;
        }
        Ok(())
    }
    #[inline]
    pub(crate) fn writable_log_offset(&self) -> usize {
        self.writable_log_offset.load(Ordering::SeqCst)
    }
    #[inline]
    pub(crate) fn writable_log_offset_fetch_add(&self, size: usize) -> usize {
        self.writable_log_offset.fetch_add(size, Ordering::SeqCst)
    }
}
impl<F: DBFileId> LogFile<F> {
    pub(crate) fn encode_entry(&self, buf: &mut Vec<u8>, entry: &Entry, offset: usize) -> usize {
        buf.clear();
        let header = VlogEntryHeader::new(&entry);
        let mut hash_writer = HashWriter {
            writer: buf,
            hasher: crc32fast::Hasher::new(),
        };
        let header_encode = header.encode();
        let header_len = hash_writer.write(&header_encode).unwrap();

        let mut kv_buf = entry.key_ts().serialize();
        kv_buf.extend_from_slice(entry.value().as_ref());
        if let Some(e) = self.try_encrypt(&kv_buf, offset) {
            kv_buf = e;
        };
        let kv_len = hash_writer.write(&kv_buf).unwrap();

        let crc = hash_writer.hasher.finalize();
        let buf = hash_writer.writer;
        buf.put_u32(crc);
        header_len + kv_len + mem::size_of::<u32>()
    }
}

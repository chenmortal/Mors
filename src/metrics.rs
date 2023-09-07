use log::debug;
use std::{
    collections::HashMap,
    fs::{metadata, read_dir},
    path::PathBuf,
    sync::Arc,
};
use tokio::{
    select,
    sync::{RwLock, Semaphore},
};

use crate::options::Options;
lazy_static! {
    static ref LSM_SIZE: RwLock<HashMap<PathBuf, u64>> = RwLock::new(HashMap::new());
    static ref VLOG_SIZE: RwLock<HashMap<PathBuf, u64>> = RwLock::new(HashMap::new());
}

#[inline]
pub(crate) async fn set_lsm_size(enabled: bool, k: &PathBuf, v: u64) {
    if !enabled {
        return;
    }
    let mut lsm_size_w = LSM_SIZE.write().await;
    lsm_size_w.insert(k.clone(), v);
    drop(lsm_size_w)
}
#[inline]
pub(crate) async fn set_vlog_size(enabled: bool, k: &PathBuf, v: u64) {
    if !enabled {
        return;
    }
    let mut vlog_size_w = VLOG_SIZE.write().await;
    vlog_size_w.insert(k.clone(), v);
    drop(vlog_size_w)
}

#[inline]
pub(crate) async fn calculate_size(opt: &Arc<Options>) {
    // let opt = &self.opt;
    let (lsm_size, mut vlog_size) = match total_size(&opt.dir) {
        Ok(r) => r,
        Err(e) => {
            debug!("Cannot calculate_size {:?} for {}", opt.dir, e);
            (0, 0)
        }
    };
    set_lsm_size(opt.metrics_enabled, &opt.dir, lsm_size).await;
    if opt.value_dir != opt.dir {
        match total_size(&opt.value_dir) {
            Ok((_, v)) => {
                vlog_size = v;
            }
            Err(e) => {
                debug!("Cannot calculate_size {:?} for {}", opt.value_dir, e);
                vlog_size = 0;
            }
        };
    }
    set_vlog_size(opt.metrics_enabled, &opt.value_dir, vlog_size).await;
}

pub(crate) async fn update_size(opt: Arc<Options>, sem: Arc<Semaphore>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
    loop {
        select! {
            _instant=interval.tick() =>{
                calculate_size(&opt).await;
            },
            _=sem.acquire()=>{
                break;
            }
        }
    }
}
fn total_size(dir: &PathBuf) -> anyhow::Result<(u64, u64)> {
    let mut lsm_size = 0;
    let mut vlog_size = 0;
    let read_dir = read_dir(dir)?;
    for ele in read_dir {
        let entry = ele?;
        let path = entry.path();
        if path.is_dir() {
            match total_size(&path) {
                Ok((sub_lsm, sub_vlog)) => {
                    lsm_size += sub_lsm;
                    vlog_size += sub_vlog;
                }
                Err(e) => {
                    debug!(
                        "Got error while calculating total size of directory: {:?} for {}",
                        path, e
                    );
                }
            }
        } else if path.is_file() {
            let meta_data = metadata(&path)?;
            let size = meta_data.len();
            let path = path.to_string_lossy();

            if path.ends_with(".sst") {
                lsm_size += size;
            } else if path.ends_with(".vlog") {
                vlog_size += size;
            }
        }
    }
    Ok((lsm_size, vlog_size))
}


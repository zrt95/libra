use crossbeam_queue::SegQueue;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

const FLAG_NOT_EXECUTED: usize = 0;
const FLAG_EXECUTED: usize = 1;

#[cfg_attr(any(target_arch = "x86_64"), repr(align(128)))]
pub struct DepStruct {
    status: AtomicUsize,                     // true if executed, false otherwise
    dep_vec: Arc<Mutex<Option<Vec<usize>>>>, // set of txn_ids that depends on the txn, set to Option for replacing the vec inside the lock efficiently
}

impl DepStruct {
    pub fn new() -> Self {
        let dep_vec = Arc::new(Mutex::new(Some(Vec::new())));
        Self {
            status: AtomicUsize::new(FLAG_NOT_EXECUTED),
            dep_vec,
        }
    }
}

pub struct Scheduler {
    curent_idx: AtomicUsize,        // the shared txn index, A in the doc
    deps_mapping: Vec<DepStruct>, // the shared mapping from a txn to txns that are dependent on the txn, B in the doc
    txn_id_buffer: SegQueue<usize>, // the shared ring buffer, B' in the doc
    stop_when: AtomicUsize,       // recording number of txns
}

impl Scheduler {
    pub fn new(num_txns: usize) -> Self {
        let curent_idx = AtomicUsize::new(0);
        let mut deps_mapping: Vec<DepStruct> = Vec::new();
        for _i in 0..num_txns {
            let dep = DepStruct::new();
            deps_mapping.push(dep);
        }
        let txn_id_buffer = SegQueue::new();
        let stop_when = AtomicUsize::new(num_txns);
        Self {
            curent_idx,
            deps_mapping,
            txn_id_buffer,
            stop_when,
        }
    }

    // Returns the next txn id for the thread to execute,
    // first fetch from the shared queue that stores dependency-resolved txn_ids,
    // then fetch from the original txn list.
    // Returns Some(id) if found the next transaction, else return None.
    pub fn next_task(&self) -> Option<usize> {
        match self.txn_id_buffer.pop() {
            // Fetch txn from B'
            Some(idx) => return Some(idx),
            None => {
                // Fetch txn from A
                let idx = self.curent_idx.fetch_add(1, Ordering::Relaxed);
                if idx >= self.stop_when.load(Ordering::Relaxed) {
                    return None;
                }
                return Some(idx);
            }
        };
    }

    // Invoked when txn idx depends on txn dep_id,
    // will add txn idx to the dependency list of txn dep_id.
    // Return Some(true) if successful, otherwise txn dep_id are executed, return None.
    pub fn update_read_deps(&self, idx: usize, dep_id: usize) -> Option<bool> {
        let status = self.deps_mapping[dep_id].status.load(Ordering::Relaxed);
        if status == FLAG_EXECUTED {
            return None;
        }
        // The data structures of dep_vec are initialized for all txn_ids, so unwrap() is safe.
        let mut deps = self.deps_mapping[dep_id].dep_vec.lock().unwrap();
        let status = self.deps_mapping[dep_id].status.load(Ordering::Relaxed);
        if status == FLAG_NOT_EXECUTED {
            deps.as_mut().unwrap().push(idx);
            return Some(true);
        }
        return None;
    }

    // After txn idx is executed, add the list of txns that depends on txn idx to the shared queue.
    pub fn update_after_execution(&self, idx: usize) {
        let mut txn_set = Some(Vec::new());
        self.deps_mapping[idx]
            .status
            .store(FLAG_EXECUTED, Ordering::Relaxed);
        {
            // we want to make things fast inside the lock, so use replace instead of clone
            let mut deps = self.deps_mapping[idx].dep_vec.lock().unwrap();
            if !deps.as_mut().unwrap().is_empty() {
                txn_set = deps.replace(Vec::new());
            }
        }
        let txns = txn_set.as_mut().unwrap();
        txns.sort();
        for ids in txns {
            self.txn_id_buffer.push(*ids);
        }
    }

    // Reset the txn version/id to end execution earlier
    pub fn set_stop_version(&self, num: usize) {
        self.stop_when.fetch_min(num, Ordering::SeqCst);
    }

    // Get the last txn version/id
    pub fn get_txn_num(&self) -> usize {
        return self.stop_when.load(Ordering::SeqCst);
    }

    pub fn print_info(&self) {
        let curent_idx = self.curent_idx.load(Ordering::Relaxed);
        let txn_num = self.stop_when.load(Ordering::Relaxed);
        let buffer_len = self.txn_id_buffer.len();
        println!(
            "Scheduler data after execution: curent_idx {}, txn_num {}, buffer_len {}",
            curent_idx, txn_num, buffer_len
        );
        for i in 0..self.deps_mapping.len() {
            let dep_struct = &self.deps_mapping[i];
            let status = dep_struct.status.load(Ordering::Relaxed);
            let deps = dep_struct.dep_vec.lock().unwrap();
            if status != FLAG_EXECUTED || deps.as_ref().unwrap().len() != 0 {
                println!(
                    "ERROR: deps_mapping for txn_id {} has status {} has length {} deps {:?}",
                    i,
                    status,
                    deps.as_ref().unwrap().len(),
                    deps
                );
            }
        }
    }
}

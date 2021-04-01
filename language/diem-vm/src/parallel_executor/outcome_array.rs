// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use diem_types::{
    transaction::{TransactionOutput, TransactionStatus},
    write_set::WriteSet,
};
use move_core_types::vm_status::VMStatus;
use std::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
};

unsafe impl Send for OutcomeArray {}
unsafe impl Sync for OutcomeArray {}

pub(crate) struct OutcomeArray {
    results: Vec<UnsafeCell<(VMStatus, TransactionOutput)>>,
}

impl OutcomeArray {
    pub fn new(len: usize) -> OutcomeArray {
        OutcomeArray {
            results: (0..len)
                .map(|_| {
                    UnsafeCell::new((
                        VMStatus::Executed,
                        TransactionOutput::new(
                            WriteSet::default(),
                            vec![],
                            0,
                            TransactionStatus::Retry,
                        ),
                    ))
                })
                .collect(),
        }
    }

    pub fn set_result(&self, idx: usize, res: (VMStatus, TransactionOutput), success: bool) {
        // Only one thread can write at the time, so just set it.
        let entry = &self.results[idx];
        unsafe {
            let mut_entry = &mut *entry.get();
            *mut_entry = res;
        }
    }

    /// Returns a vec with all results, up the the length for which results were
    //  reliably computed.
    pub fn get_all_results(self, valid_length : usize) -> Result<Vec<(VMStatus, TransactionOutput)>, VMStatus> {
        let results = self.results;
        let mut final_results = unsafe {
            // This is safe since UnsafeCell has no runtime representation.
            std::mem::transmute::<
                Vec<UnsafeCell<(VMStatus, TransactionOutput)>>,
                Vec<(VMStatus, TransactionOutput)>,
            >(results)
        };
        let _ = final_results.split_off(valid_length);
        Ok(final_results)
    }
}

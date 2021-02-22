// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::parallel_executor::dependency_analyzer::TransactionParameters;
use crate::{
    data_cache::StateViewCache,
    diem_transaction_executor::{preprocess_transaction, PreprocessedTransaction},
    logging::AdapterLogSchema,
    parallel_executor::{
        data_cache::{VersionedDataCache, VersionedStateView},
        dependency_analyzer::DependencyAnalyzer,
        outcome_array::OutcomeArray,
    },
    DiemVM,
};
use diem_state_view::StateView;
use diem_types::{
    access_path::AccessPath,
    transaction::{Transaction, TransactionOutput},
    write_set::WriteOp,
};
use move_core_types::vm_status::VMStatus;
use num_cpus;
use rayon::{prelude::*, scope};
use std::{
    cmp::{max, min},
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};

pub struct ParallelTransactionExecutor {
    num_cpus: usize,
    txn_per_thread: u64,
}

impl ParallelTransactionExecutor {
    pub fn new() -> Self {
        Self {
            num_cpus: num_cpus::get(),
            txn_per_thread: 50,
        }
    }

    pub fn execute_transactions_parallel(
        &self,
        transactions: Vec<Transaction>,
        data_cache: &mut StateViewCache,
    ) -> Result<Vec<(VMStatus, TransactionOutput)>, VMStatus> {
        let num_txns = transactions.len();
        let chunks = max(1, num_txns / self.num_cpus);

        // Update the dependency analysis structure. We only do this for blocks that
        // purely consist of UserTransactions (Extending this is a TODO). If non-user
        // transactions are detected this returns and err, and we revert to sequential
        // block processing.

        let inferer = DependencyAnalyzer::new_from_transactions(&transactions, data_cache);
        let read_write_infer = match inferer {
            Err(_) => return DiemVM::new(data_cache).execute_block_impl(transactions, data_cache),
            Ok(val) => val,
        };

        let signature_verified_block: Vec<(TransactionParameters, PreprocessedTransaction)>;
        {
            // Verify the signatures of all the transactions in parallel.
            // This is time consuming so don't wait and do the checking
            // sequentially while executing the transactions.
            signature_verified_block = transactions
                .into_par_iter()
                .map(|txn| {
                    (
                        TransactionParameters::new_from(&txn),
                        preprocess_transaction(txn),
                    )
                })
                .collect();
        }

        let infer_result: Vec<_> = {
            signature_verified_block
                .par_iter()
                .with_min_len(chunks)
                .map(|(args, txn)| read_write_infer.get_inferred_read_write_set(txn, args))
                .collect::<Result<Vec<_>, VMStatus>>()?
        };

        // Analyse each user script for its write-set and create the placeholder structure
        // that allows for parallel execution.
        let path_version_tuples: Vec<(AccessPath, usize)> = infer_result
            .par_iter()
            .enumerate()
            .with_min_len(chunks)
            .try_fold(
                || Vec::new(),
                |mut acc, (idx, (_, txn_writes))| {
                    acc.extend(txn_writes.map(|ap| (ap, idx)));
                    Ok(acc)
                },
            )
            .try_reduce(
                || Vec::new(),
                |mut lhs, mut rhs| {
                    lhs.append(&mut rhs);
                    Ok(lhs)
                },
            )?;

        let ((max_dependency_level, versioned_data_cache), outcomes) = rayon::join(
            || VersionedDataCache::new(path_version_tuples),
            || OutcomeArray::new(num_txns),
        );

        let curent_idx = AtomicUsize::new(0);
        let stop_when = signature_verified_block.len();

        scope(|s| {
            // How many threads to use?
            let compute_cpus = min(1 + (num_txns / 50), self.num_cpus - 1); // Ensure we have at least 50 tx per thread.
            let compute_cpus = min(num_txns / max_dependency_level, compute_cpus); // Ensure we do not higher rate of conflict than concurrency.

            println!(
                "Launching {} threads to execute (Max conflict {}) ... total txns: {:?}",
                compute_cpus, max_dependency_level, stop_when
            );
            for _ in 0..(compute_cpus) {
                s.spawn(|_| {
                    // Make a new VM per thread -- with its own module cache
                    let thread_vm = DiemVM::new(data_cache);

                    let mut tx_idx_ring_buffer = VecDeque::with_capacity(10);

                    loop {
                        if tx_idx_ring_buffer.len() < 10 {
                            // How many transactions to have in the buffer.

                            let idx = curent_idx.fetch_add(1, Ordering::Relaxed);
                            if idx < stop_when {
                                let (_, txn) = &signature_verified_block[idx];
                                let (reads, writes) = infer_result[idx];

                                tx_idx_ring_buffer.push_back((idx, txn, (reads, writes)));
                            }
                        }
                        if tx_idx_ring_buffer.len() == 0 {
                            break;
                        }

                        let (idx, txn, (reads, writes)) =
                            tx_idx_ring_buffer.pop_front().unwrap(); // safe due to previous check

                        let versioned_state_view =
                            VersionedStateView::new(idx, data_cache, &versioned_data_cache);

                        // Delay and move to next tx if cannot execure now.
                        if reads.clone().any(|k| versioned_state_view.will_read_block(&k)) {
                            tx_idx_ring_buffer.push_back((idx, txn, (reads, writes)));

                            // This causes a PAUSE on an x64 arch, and takes 140 cycles. Allows other
                            // core to take resources and better HT.
                            ::std::sync::atomic::spin_loop_hint();
                            continue;
                        }

                        // Execute the transaction
                        let log_context = AdapterLogSchema::new(versioned_state_view.id(), idx);
                        let res = thread_vm.execute_single_transaction(
                            txn,
                            &versioned_state_view,
                            &log_context,
                        );
                        match res {
                            Ok((vm_status, output, _sender)) => {
                                let success = !output.status().is_discarded();
                                versioned_data_cache.apply_output(&output, idx, writes);
                                outcomes.set_result(idx, (vm_status, output), success);
                            }
                            Err(_e) => {
                                panic!("TODO STOP VM & RETURN ERROR");
                            }
                        }
                    }
                });
            }
        });

        let all_results = outcomes.get_all_results();

        drop(infer_result);

        // Dropping large structures is expensive -- do this is a separate thread.
        ::std::thread::spawn(move || {
            drop(signature_verified_block); // Explicit drops to measure their cost.
            drop(versioned_data_cache);
        });

        all_results
    }
}

// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::dynamic_analysis::{concretize, ConcretizedSecondaryIndexes, ConcretizedFormals};
use anyhow::{anyhow, bail, Result};
use move_core_types::{
    account_address::AccountAddress,
    identifier::{IdentStr, Identifier},
    language_storage::{ModuleId, ResourceKey, TypeTag},
    resolver::MoveResolver,
};
use move_binary_format::layout::ModuleCache;
use read_write_set_types::ReadWriteSet;
use std::collections::BTreeMap;

pub struct NormalizedReadWriteSetAnalysis(BTreeMap<ModuleId, BTreeMap<Identifier, ReadWriteSet>>);

impl NormalizedReadWriteSetAnalysis {
    pub fn new(inner: BTreeMap<ModuleId, BTreeMap<Identifier, ReadWriteSet>>) -> Self {
        Self(inner)
    }

    fn get_summary(&self, module: &ModuleId, fun: &IdentStr) -> Option<&ReadWriteSet> {
        self.0.get(module)?.get(fun)
    }

    /// Returns an overapproximation of the `ResourceKey`'s in global storage that will be written
    /// by `module::fun` if called with arguments `signers`, `actuals`, `type_actuals` in state
    /// `blockchain_view`.
    pub fn get_keys_written<R: MoveResolver>(
        &self,
        module: &ModuleId,
        fun: &IdentStr,
        signers: &[AccountAddress],
        actuals: &[Vec<u8>],
        type_actuals: &[TypeTag],
        blockchain_view: &ModuleCache<R>,
    ) -> Result<Vec<ResourceKey>> {
        self.get_concretized_keys(
            module,
            fun,
            signers,
            actuals,
            type_actuals,
            blockchain_view,
            true,
        )
    }

    /// Returns an overapproximation of the `ResourceKey`'s in global storage that will be read by
    /// `module::fun` if called with arguments `signers`, `actuals`, `type_actuals` in state
    /// `blockchain_view`.
    pub fn get_keys_read<R: MoveResolver>(
        &self,
        module: &ModuleId,
        fun: &IdentStr,
        signers: &[AccountAddress],
        actuals: &[Vec<u8>],
        type_actuals: &[TypeTag],
        blockchain_view: &ModuleCache<R>,
    ) -> Result<Vec<ResourceKey>> {
        self.get_concretized_keys(
            module,
            fun,
            signers,
            actuals,
            type_actuals,
            blockchain_view,
            false,
        )
    }

    /// Returns an overapproximation of the `ResourceKey`'s in global storage that will be accesses
    /// by module::fun` if called with arguments `signers`, `actuals`, `type_actuals` in state
    /// `blockchain_view`.
    /// If `is_write` is true, only ResourceKey's written will be returned; otherwise, only
    /// ResourceKey's read will be returned.
    pub fn get_concretized_keys<R: MoveResolver>(
        &self,
        module: &ModuleId,
        fun: &IdentStr,
        signers: &[AccountAddress],
        actuals: &[Vec<u8>],
        type_actuals: &[TypeTag],
        blockchain_view: &ModuleCache<R>,
        is_write: bool,
    ) -> Result<Vec<ResourceKey>> {
        if let Some(state) = self.get_summary(module, fun) {
            let results = concretize(
                state,
                module,
                fun,
                signers,
                actuals,
                type_actuals,
                blockchain_view,
            )?;
            Ok(if is_write {
                results
                    .get_keys_written()
                    .ok_or_else(|| anyhow!("Failed to get keys written"))?
            } else {
                results
                    .get_keys_read()
                    .ok_or_else(|| anyhow!("Failed to get keys read"))?
            })
        } else {
            bail!("Couldn't resolve function {:?}::{:?}", module, fun)
        }
    }

    /// Returns an overapproximation of the access paths in global storage that will be read/written
    /// by `module::fun` if called with arguments `signers`, `actuals`, `type_actuals` in state
    /// `blockchain_view`.
    pub fn get_concretized_summary<R: MoveResolver>(
        &self,
        module: &ModuleId,
        fun: &IdentStr,
        signers: &[AccountAddress],
        actuals: &[Vec<u8>],
        type_actuals: &[TypeTag],
        blockchain_view: &ModuleCache<R>,
    ) -> Result<ConcretizedFormals> {
        let state = self
            .get_summary(module, fun)
            .ok_or_else(|| anyhow!("Function {}::{} to found", module, fun))?;
        concretize(
            state,
            module,
            fun,
            signers,
            actuals,
            type_actuals,
            blockchain_view,
        )
    }

    pub fn get_canonical_summary(&self, module: &ModuleId, fun: &IdentStr) -> Option<ReadWriteSet> {
        self.get_summary(module, fun).cloned()
    }

    /// Return `true` if `module`::`fun` may read an address from the blockchain state and
    /// subsequently read/write a resource stored at that address. Return `false` if the function
    /// will not do this in any possible concrete execution. Return an error if `module`::`fun` does
    /// not exist.
    pub fn may_have_secondary_indexes(&self, module: &ModuleId, fun: &IdentStr) -> Result<bool> {
        let state = self
            .get_summary(module, fun)
            .ok_or_else(|| anyhow!("Function {}::{} to found", module, fun))?;
        let mut has_secondary_index = false;
        state.iter_paths(|offset, _| {
            if offset.has_secondary_index() {
                has_secondary_index = true;
            }
            Some(())
        });
        Ok(has_secondary_index)
    }
}

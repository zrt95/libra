// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

pub mod dynamic_analysis;
pub mod normalize;

use crate::normalize::NormalizedReadWriteSetAnalysis;
use anyhow::Result;
use move_binary_format::file_format::CompiledModule;
use move_bytecode_utils::Modules;
use move_core_types::{
    identifier::{IdentStr, Identifier},
    language_storage::ModuleId,
};
use move_model::model::{FunctionEnv, GlobalEnv};
use prover_bytecode::{
    function_target_pipeline::{FunctionTargetPipeline, FunctionTargetsHolder, FunctionVariant},
    read_write_set_analysis::{ReadWriteSetProcessor, ReadWriteSetState},
};
use read_write_set_types::ReadWriteSet;
use std::collections::{BTreeMap, HashSet};

pub struct ReadWriteSetAnalysis {
    targets: FunctionTargetsHolder,
    env: GlobalEnv,
}

/// Infer read/write set results for `modules`.
/// The `modules` list must be topologically sorted by the dependency relation
/// (i.e., a child node in the dependency graph should appear earlier in the
/// vector than its parents), and all dependencies of each module must be
/// included.
pub fn analyze<'a>(
    modules: impl IntoIterator<Item = &'a CompiledModule>,
) -> Result<ReadWriteSetAnalysis> {
    let module_map = Modules::new(modules);
    let dep_graph = module_map.compute_dependency_graph();
    let topo_order = dep_graph.compute_topological_order()?;
    analyze_sorted(topo_order)
}

/// Like analyze_unsorted, but assumes that `modules` is already topologically sorted
pub fn analyze_sorted<'a>(
    modules: impl IntoIterator<Item = &'a CompiledModule>,
) -> Result<ReadWriteSetAnalysis> {
    let env = move_model::run_bytecode_model_builder(modules)?;
    let mut pipeline = FunctionTargetPipeline::default();
    pipeline.add_processor(ReadWriteSetProcessor::new());
    let mut targets = FunctionTargetsHolder::default();
    for module_env in env.get_modules() {
        for func_env in module_env.get_functions() {
            targets.add_target(&func_env)
        }
    }
    pipeline.run(&env, &mut targets);

    Ok(ReadWriteSetAnalysis { targets, env })
}

impl ReadWriteSetAnalysis {
    /// Return an overapproximation access paths read/written by `module`::`fun`.
    /// Returns `None` if the function or module does not exist.
    pub fn get_summary(&self, module: &ModuleId, fun: &IdentStr) -> Option<&ReadWriteSetState> {
        self.get_function_env(module, fun)
            .map(|fenv| {
                self.targets
                    .get_data(&fenv.get_qualified_id(), &FunctionVariant::Baseline)
                    .map(|data| data.annotations.get::<ReadWriteSetState>())
                    .flatten()
            })
            .flatten()
    }

    /// Returns the FunctionEnv for `module`::`fun`
    /// Returns `None` if this function does not exist
    pub fn get_function_env(&self, module: &ModuleId, fun: &IdentStr) -> Option<FunctionEnv> {
        self.env
            .find_function_by_language_storage_id_name(module, &fun.to_owned())
    }

    pub fn normalize(
        &self,
        add_ons: Vec<(ModuleId, Identifier)>,
    ) -> NormalizedReadWriteSetAnalysis {
        let add_on_table = add_ons.into_iter().collect::<HashSet<_>>();
        let mut result: BTreeMap<ModuleId, BTreeMap<Identifier, ReadWriteSet>> = BTreeMap::new();
        for module in self.env.get_modules() {
            let module_id = module.get_verified_module().self_id();
            let module_entry = result.entry(module_id.clone()).or_default();
            for func in module.get_functions() {
                let func_name = func.get_identifier();
                if func.is_script()
                    || add_on_table.contains(&(module_id.clone(), func_name.clone()))
                {
                    module_entry.insert(
                        func.get_identifier(),
                        self.targets
                            .get_data(&func.get_qualified_id(), &FunctionVariant::Baseline)
                            .map(|data| data.annotations.get::<ReadWriteSetState>())
                            .flatten()
                            .unwrap()
                            .normalize(&self.env),
                    );
                }
            }
        }
        NormalizedReadWriteSetAnalysis::new(result)
    }
}

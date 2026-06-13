use globset::{Glob, GlobSet, GlobSetBuilder};
use open_kioku_config::ArchitecturePolicy;
use open_kioku_core::{
    PolicyComponentMatch, ResolvedArchitectureNode, SymbolId, UnmappedPolicyTarget,
};
use open_kioku_errors::{OkError, Result};
use std::path::Path;

struct CompiledGlob {
    component_id: String,
    glob_pattern: String,
}

pub struct PolicyResolver {
    glob_set: GlobSet,
    mapping: Vec<CompiledGlob>,
}

impl PolicyResolver {
    pub fn new(policy: &ArchitecturePolicy) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        let mut mapping = Vec::new();

        // Compile layer globs
        for layer in &policy.layers {
            for path in &layer.paths {
                builder.add(Glob::new(path).map_err(|e| {
                    OkError::Config(format!(
                        "Invalid glob '{}' in layer '{}': {}",
                        path, layer.id, e
                    ))
                })?);
                mapping.push(CompiledGlob {
                    component_id: layer.id.clone(),
                    glob_pattern: path.clone(),
                });
            }
        }

        // Compile context globs
        for context in &policy.contexts {
            for path in &context.paths {
                builder.add(Glob::new(path).map_err(|e| {
                    OkError::Config(format!(
                        "Invalid glob '{}' in context '{}': {}",
                        path, context.id, e
                    ))
                })?);
                mapping.push(CompiledGlob {
                    component_id: context.id.clone(),
                    glob_pattern: path.clone(),
                });
            }
        }

        let glob_set = builder
            .build()
            .map_err(|e| OkError::Config(format!("Failed to build policy globset: {}", e)))?;

        Ok(Self { glob_set, mapping })
    }

    /// Resolves components for a specific file path.
    pub fn resolve_file(&self, file_path: impl AsRef<Path>) -> Vec<PolicyComponentMatch> {
        let path = file_path.as_ref();
        let matches = self.glob_set.matches(path);
        let mut resolved = matches
            .into_iter()
            .map(|index| {
                let compiled = &self.mapping[index];
                PolicyComponentMatch {
                    component_id: compiled.component_id.clone(),
                    matched_glob: compiled.glob_pattern.clone(),
                }
            })
            .collect::<Vec<_>>();

        // Ensure deterministic order by component ID, then glob pattern
        resolved.sort_by(|a, b| {
            a.component_id
                .cmp(&b.component_id)
                .then_with(|| a.matched_glob.cmp(&b.matched_glob))
        });
        resolved
    }

    /// Helper to resolve a node (either file or symbol in file).
    pub fn resolve_node(
        &self,
        file_path: impl AsRef<Path>,
        symbol_id: Option<SymbolId>,
    ) -> std::result::Result<ResolvedArchitectureNode, UnmappedPolicyTarget> {
        let path = file_path.as_ref();
        let components = self.resolve_file(path);

        if components.is_empty() {
            Err(UnmappedPolicyTarget {
                file_path: path.to_path_buf(),
                symbol_id,
            })
        } else {
            Ok(ResolvedArchitectureNode {
                file_path: path.to_path_buf(),
                symbol_id,
                components,
            })
        }
    }
}

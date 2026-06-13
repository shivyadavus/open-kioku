pub mod resolver;

pub use resolver::PolicyResolver;

use open_kioku_core::{ArchitectureComponent, UnmappedPolicyTarget};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub components: Vec<ArchitectureComponent>,
    pub unmapped_targets: Vec<UnmappedPolicyTarget>,
    pub violations: Vec<String>,
}

pub struct ArchitectureDetector<'a> {
    store: &'a dyn MetadataStore,
    resolver: Option<&'a PolicyResolver>,
}

impl<'a> ArchitectureDetector<'a> {
    pub fn new(store: &'a dyn MetadataStore, resolver: Option<&'a PolicyResolver>) -> Self {
        Self { store, resolver }
    }

    pub fn detect(&self) -> Result<ArchitectureSummary> {
        let files = self.store.list_files(usize::MAX, 0)?;

        let mut unmapped_targets = Vec::new();

        if let Some(resolver) = self.resolver {
            let mut component_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

            for file in files {
                let path = file.path.display().to_string();
                let resolution = resolver.resolve_node(&path, None);
                match resolution {
                    Ok(node) => {
                        for comp in node.components {
                            component_map
                                .entry(comp.component_id)
                                .or_default()
                                .push(path.clone());
                        }
                    }
                    Err(unmapped) => {
                        unmapped_targets.push(unmapped);
                    }
                }
            }

            let components = component_map
                .into_iter()
                .map(|(id, mut paths)| {
                    paths.sort();
                    paths.dedup();
                    ArchitectureComponent {
                        id: id.clone(),
                        name: id,
                        paths,
                        evidence: Vec::new(),
                    }
                })
                .collect();

            Ok(ArchitectureSummary {
                components,
                unmapped_targets,
                violations: Vec::new(),
            })
        } else {
            // Fallback for missing policy (or for test compatibility)
            let mut components = Vec::new();
            for (name, marker) in [
                ("api", "/api/"),
                ("service", "/service"),
                ("repository", "/repo"),
                ("tests", "/test"),
                ("configuration", "config"),
            ] {
                let paths = files
                    .iter()
                    .filter(|file| {
                        file.path
                            .to_string_lossy()
                            .to_ascii_lowercase()
                            .contains(marker)
                    })
                    .map(|file| file.path.display().to_string())
                    .collect::<Vec<_>>();
                if !paths.is_empty() {
                    components.push(ArchitectureComponent {
                        id: name.into(),
                        name: name.into(),
                        paths,
                        evidence: Vec::new(),
                    });
                }
            }
            Ok(ArchitectureSummary {
                components,
                unmapped_targets,
                violations: Vec::new(),
            })
        }
    }
}

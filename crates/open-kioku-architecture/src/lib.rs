use open_kioku_core::ArchitectureComponent;
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchitectureSummary {
    pub components: Vec<ArchitectureComponent>,
    pub violations: Vec<String>,
}

pub struct ArchitectureDetector<'a> {
    store: &'a dyn MetadataStore,
}

impl<'a> ArchitectureDetector<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self { store }
    }

    pub fn detect(&self) -> Result<ArchitectureSummary> {
        let files = self.store.list_files(usize::MAX, 0)?;
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
            violations: Vec::new(),
        })
    }
}

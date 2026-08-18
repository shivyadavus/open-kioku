use crate::semantics::LanguageSemantics;
use open_kioku_core::Language;

pub struct PythonSemantics;

impl LanguageSemantics for PythonSemantics {
    fn language(&self) -> Language {
        Language::Python
    }

    fn module_separator(&self) -> &'static str {
        "."
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["self", "cls"]
    }
}

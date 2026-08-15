use crate::semantics::LanguageSemantics;
use open_kioku_core::Language;

pub struct GoSemantics;

impl LanguageSemantics for GoSemantics {
    fn language(&self) -> Language {
        Language::Go
    }

    fn module_separator(&self) -> &'static str {
        "/"
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
}

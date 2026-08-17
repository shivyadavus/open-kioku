use crate::semantics::LanguageSemantics;
use open_kioku_core::Language;

pub struct JavaSemantics;

impl LanguageSemantics for JavaSemantics {
    fn language(&self) -> Language {
        Language::Java
    }

    fn module_separator(&self) -> &'static str {
        "."
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }

    fn implicit_self_dispatch(&self) -> bool {
        true
    }
}

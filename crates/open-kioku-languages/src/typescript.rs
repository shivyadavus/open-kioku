use crate::semantics::LanguageSemantics;
use open_kioku_core::Language;

pub struct TypeScriptSemantics;

impl LanguageSemantics for TypeScriptSemantics {
    fn language(&self) -> Language {
        Language::TypeScript
    }

    fn module_separator(&self) -> &'static str {
        "/"
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
}

pub struct JavaScriptSemantics;

impl LanguageSemantics for JavaScriptSemantics {
    fn language(&self) -> Language {
        Language::JavaScript
    }

    fn module_separator(&self) -> &'static str {
        "/"
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
}

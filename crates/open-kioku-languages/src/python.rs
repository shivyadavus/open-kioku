use crate::semantics::LanguageSemantics;
use open_kioku_core::{Language, ReceiverKind};

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

    fn classify_receiver(&self, receiver: &str) -> ReceiverKind {
        let trimmed = receiver.trim();
        if trimmed == "self" || trimmed == "cls" {
            ReceiverKind::Self_
        } else if trimmed == "super()" || trimmed == "super" {
            ReceiverKind::Super
        } else if trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            ReceiverKind::Type
        } else {
            ReceiverKind::Value
        }
    }
}

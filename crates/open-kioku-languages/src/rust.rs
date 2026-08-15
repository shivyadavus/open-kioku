use crate::semantics::LanguageSemantics;
use open_kioku_core::{Language, ReceiverKind};

pub struct RustSemantics;

impl LanguageSemantics for RustSemantics {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn module_separator(&self) -> &'static str {
        "::"
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["self", "Self"]
    }

    fn classify_receiver(&self, receiver: &str) -> ReceiverKind {
        let trimmed = receiver.trim();
        if trimmed == "self" || trimmed == "Self" {
            ReceiverKind::Self_
        } else if trimmed == "super" {
            ReceiverKind::Super
        } else if trimmed == "crate" {
            ReceiverKind::Module
        } else if trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            ReceiverKind::Type
        } else {
            ReceiverKind::Value
        }
    }
}

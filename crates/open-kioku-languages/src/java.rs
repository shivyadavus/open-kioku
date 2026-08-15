use crate::semantics::LanguageSemantics;
use open_kioku_core::{Language, ReceiverKind};

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

    fn classify_receiver(&self, receiver: &str) -> ReceiverKind {
        let trimmed = receiver.trim();
        if trimmed == "this" {
            ReceiverKind::Self_
        } else if trimmed == "super" {
            ReceiverKind::Super
        } else if trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
            ReceiverKind::Type
        } else {
            ReceiverKind::Value
        }
    }
}

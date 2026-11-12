use crate::semantics::LanguageSemantics;
use open_kioku_core::{Language, ReceiverKind};

pub struct CSharpSemantics;

impl LanguageSemantics for CSharpSemantics {
    fn language(&self) -> Language {
        Language::CSharp
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

    /// `base.M()` is the parent's member. A capitalised name is not known to be a type, as it
    /// is in Java: properties and fields are capitalised too (`Client.Execute()` may read the
    /// property `Client`), so only resolution, which knows what the name is bound to, decides.
    fn classify_receiver(&self, receiver: &str) -> ReceiverKind {
        let trimmed = receiver.trim();
        if trimmed == "this" {
            ReceiverKind::Self_
        } else if trimmed == "base" {
            ReceiverKind::Super
        } else {
            ReceiverKind::Value
        }
    }
}

use open_kioku_core::{GraphEdgeType, Language, ReceiverKind, SymbolKind, Visibility};

pub trait LanguageSemantics: Send + Sync {
    fn language(&self) -> Language;
    fn module_separator(&self) -> &'static str;
    fn self_receivers(&self) -> &'static [&'static str];
    fn implicit_self_dispatch(&self) -> bool {
        false
    }

    fn classify_receiver(&self, receiver: &str) -> ReceiverKind {
        let trimmed = receiver.trim();
        if self.self_receivers().contains(&trimmed) {
            ReceiverKind::Self_
        } else if trimmed == "super" || trimmed == "Super" {
            ReceiverKind::Super
        } else if trimmed
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            ReceiverKind::Type
        } else {
            ReceiverKind::Value
        }
    }

    fn visibility_default(&self) -> Visibility {
        Visibility::Public
    }

    fn compatible_relationship(
        &self,
        _from: SymbolKind,
        to: SymbolKind,
        edge: GraphEdgeType,
    ) -> bool {
        match edge {
            GraphEdgeType::Calls => matches!(
                to,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Class | SymbolKind::Unknown
            ),
            GraphEdgeType::Extends | GraphEdgeType::Implements => matches!(
                to,
                SymbolKind::Class | SymbolKind::Interface | SymbolKind::Trait | SymbolKind::Unknown
            ),
            _ => true,
        }
    }

    fn private_member_cross_file_allowed(&self) -> bool {
        false
    }
}

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
}

/// The condition of a `#[cfg_attr(condition, ..)]` attribute, with whitespace outside string
/// literals removed so two spellings of one condition compare equal. `None` for any other
/// attribute, or one this cannot read exactly: a comment, a raw or byte string, an escape, a
/// key whose value is not a string literal, or an unbalanced delimiter.
pub fn cfg_attr_condition(attribute: &str) -> Option<String> {
    let text = normalize_attribute(attribute)?;
    let arguments = text.strip_prefix("#[cfg_attr(")?.strip_suffix(")]")?;
    let mut depth = 0usize;
    let mut in_string = false;
    for (at, ch) in arguments.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            _ if in_string => {}
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            // `feature = "a"`: a value that is not a plain literal cannot be compared.
            '=' if !arguments[at + 1..].starts_with('"') => return None,
            ',' if depth == 0 => {
                let condition = &arguments[..at];
                if condition.is_empty() {
                    return None;
                }
                // The `path` must be set by this attribute's condition alone: one inside a
                // nested `cfg_attr(unix, cfg_attr(feature = "x", path = ..))` holds on fewer
                // builds than the outer condition says.
                let items = top_level_items(&arguments[at + 1..])?;
                if items.iter().any(|item| item.starts_with("cfg_attr("))
                    || !items.iter().any(|item| item.starts_with("path=\""))
                {
                    return None;
                }
                return Some(condition.to_string());
            }
            _ => {}
        }
    }
    None
}

/// The comma-separated items of `text` outside brackets and literals, or `None` when a bracket
/// does not close.
fn top_level_items(text: &str) -> Option<Vec<&str>> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut from = 0;
    for (at, ch) in text.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            _ if in_string => {}
            '(' => depth += 1,
            ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                items.push(&text[from..at]);
                from = at + 1;
            }
            _ => {}
        }
    }
    items.push(&text[from..]);
    (depth == 0).then_some(items)
}

/// Whether one of `conditions`, each as [`cfg_attr_condition`] reads it, holds on every build:
/// `all()`, or a condition written beside its own `not(..)`. Only those are recognised, which
/// compare the conditions as written: `any(unix, not(unix))` or `unix` beside `not(any(unix))`
/// reads as a condition that may fail.
pub fn cfg_conditions_hold_on_every_build(conditions: &[String]) -> bool {
    conditions.iter().any(|condition| {
        condition == "all()"
            || conditions
                .iter()
                .any(|other| *other == format!("not({condition})"))
    })
}

/// `attribute` without whitespace outside its string literals, when it holds no comment, raw or
/// byte string, character literal or escape and its literals and brackets close.
fn normalize_attribute(attribute: &str) -> Option<String> {
    let mut out = String::with_capacity(attribute.len());
    let mut in_string = false;
    let mut previous = None;
    let mut depth = 0usize;
    for ch in attribute.chars() {
        if in_string {
            if ch == '\\' {
                return None;
            }
            if ch == '"' {
                in_string = false;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                // `r"..."`, `b"..."`: a prefix the comparison does not read.
                if previous.is_some_and(|ch: char| ch == '_' || ch.is_alphanumeric()) {
                    return None;
                }
                in_string = true;
            }
            '\'' => return None,
            '/' if matches!(previous, Some('/')) => return None,
            '*' if matches!(previous, Some('/')) => return None,
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        if !ch.is_whitespace() {
            out.push(ch);
            previous = Some(ch);
        } else {
            previous = None;
        }
    }
    (!in_string && depth == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conditions(attributes: &[&str]) -> Vec<String> {
        attributes
            .iter()
            .filter_map(|attribute| cfg_attr_condition(attribute))
            .collect()
    }

    #[test]
    fn cfg_attr_condition_is_read_without_whitespace_outside_literals() {
        assert_eq!(
            cfg_attr_condition("#[cfg_attr( all( unix , feature = \"a b\" ),\n path = \"x.rs\")]")
                .as_deref(),
            Some("all(unix,feature=\"a b\")")
        );
        assert_eq!(
            cfg_attr_condition("#[cfg_attr(not(unix), path = \"x.rs\", allow(dead_code))]")
                .as_deref(),
            Some("not(unix)")
        );
        for unreadable in [
            "#[path = \"x.rs\"]",
            "#[cfg_attr(unix)]",
            "#[cfg_attr(, path = \"x.rs\")]",
            "#[cfg_attr(feature = r\"a\", path = \"x.rs\")]",
            "#[cfg_attr(feature = \"a\\\"\", path = \"x.rs\")]",
            "#[cfg_attr(unix /* why */, path = \"x.rs\")]",
            "#[cfg_attr(all(unix, path = \"x.rs\")]",
            "#[cfg_attr(feature = , path = \"x.rs\")]",
            // The `path` is set on a narrower condition than the outer one, or not at all.
            "#[cfg_attr(unix, cfg_attr(feature = \"x\", path = \"a.rs\"))]",
            "#[cfg_attr(unix, path = \"a.rs\", cfg_attr(feature = \"x\", path = \"b.rs\"))]",
            "#[cfg_attr(unix, doc = \"path = x\")]",
        ] {
            assert_eq!(cfg_attr_condition(unreadable), None, "{unreadable}");
        }
    }

    #[test]
    fn only_all_and_a_condition_beside_its_negation_hold_on_every_build() {
        for total in [
            &["#[cfg_attr(all(), path = \"x.rs\")]"][..],
            &["#[cfg_attr( all( ) , path = \"x.rs\")]"],
            &[
                "#[cfg_attr(unix, path = \"u.rs\")]",
                "#[cfg_attr(not(unix), path = \"o.rs\")]",
            ],
            &[
                "#[cfg_attr(not( any(unix, windows) ), path = \"o.rs\")]",
                "#[cfg_attr(windows, path = \"w.rs\")]",
                "#[cfg_attr(any(unix,windows), path = \"u.rs\")]",
            ],
            &[
                "#[cfg_attr(feature = \"a\", path = \"u.rs\")]",
                "#[cfg_attr(not(feature = \"a\"), path = \"o.rs\")]",
            ],
        ] {
            assert!(
                cfg_conditions_hold_on_every_build(&conditions(total)),
                "{total:?}"
            );
        }
        for partial in [
            &["#[cfg_attr(unix, path = \"u.rs\")]"][..],
            &["#[cfg_attr(any(), path = \"x.rs\")]"],
            &["#[cfg_attr(not(any()), path = \"x.rs\")]"],
            &[
                "#[cfg_attr(unix, path = \"u.rs\")]",
                "#[cfg_attr(windows, path = \"w.rs\")]",
            ],
            &[
                "#[cfg_attr(unix, path = \"u.rs\")]",
                "#[cfg_attr(not(not(unix)), path = \"o.rs\")]",
            ],
            &["#[cfg_attr(any(unix, not(unix)), path = \"x.rs\")]"],
            &[
                "#[cfg_attr(unix, cfg_attr(feature = \"x\", path = \"a.rs\"))]",
                "#[cfg_attr(not(unix), path = \"b.rs\")]",
            ],
            &[
                "#[cfg_attr(feature = \"a\", path = \"u.rs\")]",
                "#[cfg_attr(not(feature = \"b\"), path = \"o.rs\")]",
            ],
            &[
                "#[cfg_attr(feature = \"a b\", path = \"u.rs\")]",
                "#[cfg_attr(not(feature = \"ab\"), path = \"o.rs\")]",
            ],
        ] {
            assert!(
                !cfg_conditions_hold_on_every_build(&conditions(partial)),
                "{partial:?}"
            );
        }
    }
}

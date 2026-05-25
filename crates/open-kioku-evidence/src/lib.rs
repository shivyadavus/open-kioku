use open_kioku_core::{Confidence, Evidence};

pub fn minimum_confidence(evidence: &[Evidence]) -> Confidence {
    if evidence
        .iter()
        .any(|item| item.confidence == Confidence::Low)
    {
        Confidence::Low
    } else if evidence
        .iter()
        .any(|item| item.confidence == Confidence::Medium)
    {
        Confidence::Medium
    } else if evidence
        .iter()
        .any(|item| item.confidence == Confidence::High)
    {
        Confidence::High
    } else {
        Confidence::Exact
    }
}

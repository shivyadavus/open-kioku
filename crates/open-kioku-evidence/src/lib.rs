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

#[derive(Default)]
pub struct EvidenceBuilder {
    evidence: Vec<String>,
    scores: Vec<f32>,
}

impl EvidenceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(mut self, text: impl Into<String>, score: f32) -> Self {
        self.evidence.push(text.into());
        self.scores.push(score);
        self
    }

    pub fn build(self) -> (Vec<String>, f32) {
        if self.scores.is_empty() {
            return (self.evidence, 0.0);
        }
        let max_score = self.scores.iter().copied().fold(0.0_f32, f32::max);
        let normalized = (max_score * 100.0).round() / 100.0;
        (self.evidence, normalized)
    }
}

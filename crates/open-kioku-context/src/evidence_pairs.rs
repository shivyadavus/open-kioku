//! Evidence lines and the refs that name them travel as pairs.
//!
//! On a context result, `evidence_refs[i]` names the fact `evidence[i]` states. Every step that
//! adds or merges evidence on a result goes through this module, so a line never gains or
//! loses its ref on the way. Merging, deduplicating and sorting the two lists separately left
//! refs that named another line's fact, or no fact at all (#433).
//!
//! A line with no id of its own is named by position, `search:<path>:<range>:<index>`, which is
//! the id the pack publishes that line's record under. Until the pack is built the index is
//! only a token that keeps the result's refs unique: merging, reranking and region widening
//! move lines and change ranges. Once a primary result is final, [`publish_positional_refs`]
//! renames each positional ref to the record of its own line.

use open_kioku_core::{search_result_evidence_ids, SearchResult};

const POSITIONAL_REF_PREFIX: &str = "search:";

/// Makes `result.evidence_refs` hold exactly one ref per evidence line, each ref unique.
///
/// Refs that are not one per line cannot be matched to lines by position, so they are
/// replaced with positional ids rather than guessed. Returns whether that happened.
pub(crate) fn pair_evidence_refs(result: &mut SearchResult) -> bool {
    if result.evidence_refs.len() != result.evidence.len() {
        result.evidence_refs = if result.evidence.is_empty() {
            Vec::new()
        } else {
            search_result_evidence_ids(&result.path, &result.line_range, result.evidence.len())
        };
        return true;
    }
    let refs = std::mem::take(&mut result.evidence_refs);
    for (index, evidence_ref) in refs.into_iter().enumerate() {
        let evidence_ref = if result.evidence_refs.contains(&evidence_ref) {
            rekeyed_ref(result, &evidence_ref, index)
        } else {
            evidence_ref
        };
        result.evidence_refs.push(evidence_ref);
    }
    false
}

/// Appends `line` with the ref of the fact it states, or a positional id when the line has no
/// id of its own. A line the result already carries is not repeated.
///
/// A positional ref another line already holds is renumbered: two index hits on the same
/// chunk number their lines from zero. Any other ref the result already cites names a fact
/// the result already states, so the line is not added and the fact is not counted twice.
pub(crate) fn push_evidence(result: &mut SearchResult, line: String, evidence_ref: Option<String>) {
    pair_evidence_refs(result);
    if result.evidence.contains(&line) {
        return;
    }
    let index = result.evidence.len();
    let evidence_ref = match evidence_ref {
        Some(evidence_ref) if !result.evidence_refs.contains(&evidence_ref) => evidence_ref,
        Some(evidence_ref) if positional_prefix(&evidence_ref).is_some() => {
            rekeyed_ref(result, &evidence_ref, index)
        }
        Some(_) => return,
        None => free_positional_ref(&result.evidence_refs, &own_positional_prefix(result), index),
    };
    result.evidence.push(line);
    result.evidence_refs.push(evidence_ref);
}

/// Appends each of `source`'s lines, with its ref, that `target` does not already state.
pub(crate) fn merge_evidence(target: &mut SearchResult, source: &SearchResult) {
    let mut source_refs = source.evidence_refs.clone();
    if source_refs.len() != source.evidence.len() {
        let mut paired = source.clone();
        pair_evidence_refs(&mut paired);
        source_refs = paired.evidence_refs;
    }
    for (line, evidence_ref) in source.evidence.iter().zip(source_refs) {
        push_evidence(target, line.clone(), Some(evidence_ref));
    }
}

/// Orders the result's evidence lines by text, each keeping its ref.
pub(crate) fn sort_evidence_by_line(result: &mut SearchResult) {
    pair_evidence_refs(result);
    let mut pairs = std::mem::take(&mut result.evidence)
        .into_iter()
        .zip(std::mem::take(&mut result.evidence_refs))
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    (result.evidence, result.evidence_refs) = pairs.into_iter().unzip();
}

/// Renames every positional ref of a final primary result to the id its own line's record is
/// published under. Refs from other producers (graph edges, symbols, documents, region steps)
/// keep their ids.
pub(crate) fn publish_positional_refs(result: &mut SearchResult) {
    pair_evidence_refs(result);
    let ids = search_result_evidence_ids(&result.path, &result.line_range, result.evidence.len());
    for (evidence_ref, id) in result.evidence_refs.iter_mut().zip(ids) {
        if evidence_ref.starts_with(POSITIONAL_REF_PREFIX) {
            *evidence_ref = id;
        }
    }
}

/// Why `result`'s refs do not pair one to one with its lines, if they do not.
pub(crate) fn pairing_violation(result: &SearchResult) -> Option<String> {
    if result.evidence_refs.len() != result.evidence.len() {
        return Some(format!(
            "{} evidence line(s) but {} evidence ref(s)",
            result.evidence.len(),
            result.evidence_refs.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    result
        .evidence_refs
        .iter()
        .find(|evidence_ref| !seen.insert(evidence_ref.as_str()))
        .map(|evidence_ref| format!("evidence ref `{evidence_ref}` names more than one line"))
}

/// Checks the pairing of every result a pack publishes. A result that fails it has its refs
/// replaced with positional ids, which can name no other line's fact, and the returned caveat
/// says so rather than letting the pack publish refs that cannot be trusted.
pub(crate) fn enforce_pairing(results: &mut [SearchResult]) -> Vec<String> {
    let mut caveats = Vec::new();
    for result in results {
        if let Some(violation) = pairing_violation(result) {
            result.evidence_refs.clear();
            pair_evidence_refs(result);
            caveats.push(format!(
                "evidence refs of `{}` did not pair with its evidence lines ({violation}); they were replaced with positional ids",
                result.path.display()
            ));
        }
    }
    caveats
}

fn own_positional_prefix(result: &SearchResult) -> String {
    let range = result
        .line_range
        .as_ref()
        .map(|range| format!("{}-{}", range.start, range.end))
        .unwrap_or_else(|| "unknown".into());
    format!("{POSITIONAL_REF_PREFIX}{}:{range}", result.path.display())
}

/// The `search:<path>:<range>` part of a positional ref.
fn positional_prefix(evidence_ref: &str) -> Option<&str> {
    if !evidence_ref.starts_with(POSITIONAL_REF_PREFIX) {
        return None;
    }
    let (prefix, index) = evidence_ref.rsplit_once(':')?;
    (!index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())).then_some(prefix)
}

/// A unique ref for the line at `index` whose own ref another line already holds: the next
/// free position under the ref's chunk when it is positional, otherwise under the result's.
fn rekeyed_ref(result: &SearchResult, evidence_ref: &str, index: usize) -> String {
    let prefix = positional_prefix(evidence_ref)
        .map(str::to_string)
        .unwrap_or_else(|| own_positional_prefix(result));
    free_positional_ref(&result.evidence_refs, &prefix, index)
}

/// `<prefix>:<preferred>` when no ref holds it, otherwise the lowest free index.
fn free_positional_ref(refs: &[String], prefix: &str, preferred: usize) -> String {
    let candidate = |index: usize| format!("{prefix}:{index}");
    let taken = |id: &str| refs.iter().any(|existing| existing == id);
    let preferred_id = candidate(preferred);
    if !taken(&preferred_id) {
        return preferred_id;
    }
    (0..)
        .map(candidate)
        .find(|id| !taken(id))
        .expect("an unbounded index range always has a free index")
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::LineRange;

    fn hit(path: &str, start: u32, end: u32, lines: &[&str]) -> SearchResult {
        let mut result = SearchResult {
            path: path.into(),
            line_range: Some(LineRange { start, end }),
            snippet: String::new(),
            symbol: None,
            score: 1.0,
            match_reason: "tantivy hybrid lexical match".into(),
            evidence: lines.iter().map(|line| line.to_string()).collect(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        };
        result.reconcile_score_breakdown();
        result
    }

    #[test]
    fn two_hits_on_one_chunk_keep_one_ref_per_distinct_line() {
        let mut first = hit("src/rates.rs", 4, 9, &["bm25", "variant `rate`"]);
        let second = hit("src/rates.rs", 4, 9, &["bm25", "variant `limit`"]);
        merge_evidence(&mut first, &second);

        assert_eq!(
            first.evidence,
            vec!["bm25", "variant `rate`", "variant `limit`"]
        );
        assert_eq!(
            first.evidence_refs,
            vec![
                "search:src/rates.rs:4-9:0",
                "search:src/rates.rs:4-9:1",
                "search:src/rates.rs:4-9:2",
            ]
        );
    }

    #[test]
    fn sorting_lines_carries_each_ref_with_its_line() {
        let mut result = hit("src/rates.rs", 1, 3, &["zeta", "alpha"]);
        push_evidence(&mut result, "graph neighbor".into(), Some("edge:7".into()));
        sort_evidence_by_line(&mut result);

        assert_eq!(result.evidence, vec!["alpha", "graph neighbor", "zeta"]);
        assert_eq!(
            result.evidence_refs,
            vec![
                "search:src/rates.rs:1-3:1",
                "edge:7",
                "search:src/rates.rs:1-3:0"
            ]
        );
        publish_positional_refs(&mut result);
        assert_eq!(
            result.evidence_refs,
            vec![
                "search:src/rates.rs:1-3:0",
                "edge:7",
                "search:src/rates.rs:1-3:2"
            ]
        );
    }

    #[test]
    fn a_fact_already_cited_is_not_stated_twice() {
        let mut result = hit("src/rates.rs", 1, 3, &["bm25"]);
        push_evidence(
            &mut result,
            "runtime evidence".into(),
            Some("runtime:1".into()),
        );
        push_evidence(
            &mut result,
            "runtime corroboration".into(),
            Some("runtime:1".into()),
        );

        assert_eq!(result.evidence, vec!["bm25", "runtime evidence"]);
        assert_eq!(pairing_violation(&result), None);
    }

    #[test]
    fn refs_that_are_not_one_per_line_are_replaced_rather_than_guessed() {
        let mut result = hit("src/rates.rs", 2, 3, &["matched `rate`", "history churn"]);
        result.evidence_refs = vec![
            "history-author:abc".into(),
            "history-churn:src/rates.rs".into(),
            "history-similar:1".into(),
        ];
        assert!(pairing_violation(&result).is_some());
        assert!(pair_evidence_refs(&mut result));
        assert_eq!(
            result.evidence_refs,
            vec!["search:src/rates.rs:2-3:0", "search:src/rates.rs:2-3:1"]
        );
    }
}

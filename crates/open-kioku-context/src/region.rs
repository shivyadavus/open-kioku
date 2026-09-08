//! Region widening for the top-ranked primary files.
//!
//! Selection works on snippet-sized units (one chunk per symbol start). On commit-derived
//! holdouts from four large public repositories those units covered 3–22% of the lines the
//! real change touched even when the file was right, identical at 4k, 8k and 16k tokens,
//! with median tokens-to-first-gold of zero: the ordering was right and the cut was wrong.
//!
//! Widening therefore runs *after* selection and never reorders it. For the first
//! `ContextBudget::region_files` distinct files in selection order it grows each selected unit
//! to its enclosing symbol, re-admits the file's other task-ranked units, then absorbs
//! physically adjacent chunks, until the file reaches `region_tokens_per_file` or the context
//! budget selection left over is spent. Nothing selection chose is removed or reordered, because
//! widening runs after it and only grows or appends units - that, not the budget guard, is what
//! keeps a lower-ranked file's first unit in the pack; on the file-limit budget the CLI and MCP
//! use, the leftover budget is effectively unbounded and `region_tokens_per_file` is the only
//! bound that binds. Every step is recorded as a `region:` evidence ref on
//! the unit so the pack stays explainable, and the retrieval trace keyed on the unit follows
//! its new identity so source attribution survives.

use open_kioku_core::{
    CodeChunk, ContextBudget, File, LineRange, RetrievalDiagnostics, RetrievalUnitKey,
    SearchResult, Symbol,
};

use crate::{estimate_search_result_tokens, normalize_path};

pub(crate) const ENCLOSING_SYMBOL_REF: &str = "region:enclosing-symbol";
pub(crate) const RANKED_UNIT_REF: &str = "region:ranked-unit";
pub(crate) const ADJACENT_UNIT_REF: &str = "region:adjacent-unit";

pub(crate) fn widen_selected_regions(
    mut selected: Vec<SearchResult>,
    ranked: &[SearchResult],
    files: &[File],
    chunks: &[CodeChunk],
    symbols: &[Symbol],
    budget: &ContextBudget,
    diagnostics: &mut RetrievalDiagnostics,
) -> Vec<SearchResult> {
    if budget.region_files == 0 || budget.region_tokens_per_file == 0 || selected.is_empty() {
        return selected;
    }
    let spent = selected
        .iter()
        .map(estimate_search_result_tokens)
        .sum::<usize>();
    let mut remaining = budget.available_context_tokens().saturating_sub(spent);
    let selected_keys = selected
        .iter()
        .map(RetrievalUnitKey::from_result)
        .collect::<Vec<_>>();

    let mut region_paths = Vec::<String>::new();
    for result in &selected {
        let path = normalize_path(&result.path);
        if region_paths.len() < budget.region_files && !region_paths.contains(&path) {
            region_paths.push(path);
        }
    }

    for path in region_paths {
        let Some(file) = files.iter().find(|file| normalize_path(&file.path) == path) else {
            continue;
        };
        let mut file_chunks = chunks
            .iter()
            .filter(|chunk| chunk.file_id == file.id)
            .collect::<Vec<_>>();
        file_chunks.sort_by_key(|chunk| chunk.range.start);
        let file_symbols = symbols
            .iter()
            .filter(|symbol| symbol.file_id == file.id && symbol.range.is_some())
            .collect::<Vec<_>>();
        let mut region = FileRegion {
            path: &path,
            cap: budget.region_tokens_per_file,
            chunks: &file_chunks,
            symbols: &file_symbols,
            file_tokens: unit_indices(&selected, &path)
                .into_iter()
                .map(|index| estimate_search_result_tokens(&selected[index]))
                .sum(),
        };
        // Materialize evidence ids while the units still carry their original identity: a
        // widened range must not rewrite the ids that explain why the unit was retrieved.
        let original_keys = unit_indices(&selected, &path)
            .into_iter()
            .map(|index| {
                ensure_evidence_refs(&mut selected[index]);
                (index, RetrievalUnitKey::from_result(&selected[index]))
            })
            .collect::<Vec<_>>();

        region.widen_to_enclosing_symbols(&mut selected, &mut remaining, diagnostics);
        region.readmit_ranked_units(
            &mut selected,
            ranked,
            &selected_keys,
            &mut remaining,
            diagnostics,
        );
        region.absorb_adjacent_chunks(&mut selected, &mut remaining);

        for (index, original) in original_keys {
            let current = RetrievalUnitKey::from_result(&selected[index]);
            if current != original {
                retarget_trace(diagnostics, &original, current);
            }
        }
    }
    selected
}

/// Region-widening tags on a unit, in the order they were applied, for the selection rationale.
pub(crate) fn region_steps(result: &SearchResult) -> Vec<&str> {
    result
        .evidence_refs
        .iter()
        .filter_map(|reference| {
            [ENCLOSING_SYMBOL_REF, RANKED_UNIT_REF, ADJACENT_UNIT_REF]
                .into_iter()
                .find(|tag| reference.starts_with(tag))
        })
        .collect()
}

struct FileRegion<'a> {
    path: &'a str,
    cap: usize,
    chunks: &'a [&'a CodeChunk],
    symbols: &'a [&'a Symbol],
    file_tokens: usize,
}

impl FileRegion<'_> {
    fn widen_to_enclosing_symbols(
        &mut self,
        selected: &mut [SearchResult],
        remaining: &mut usize,
        diagnostics: &mut RetrievalDiagnostics,
    ) {
        for index in unit_indices(selected, self.path) {
            let Some(range) = selected[index].line_range.clone() else {
                continue;
            };
            let Some((symbol, symbol_range)) = enclosing_symbol(self.symbols, &range) else {
                continue;
            };
            let wanted = LineRange {
                start: range.start.min(symbol_range.start),
                end: range.end.max(symbol_range.end),
            };
            let Some((covered, snippet)) = region_snippet(self.chunks, &wanted) else {
                continue;
            };
            if covered == range {
                continue;
            }
            let mut candidate = selected[index].clone();
            let before = estimate_search_result_tokens(&candidate);
            candidate.line_range = Some(covered.clone());
            candidate.snippet = snippet;
            let delta = estimate_search_result_tokens(&candidate).saturating_sub(before);
            let location = format!("{}:{}-{}", self.path, range.start, range.end);
            if self.file_tokens.saturating_add(delta) > self.cap {
                diagnostics.selection.omitted_due_to_caps.push(format!(
                    "{location}: not widened to enclosing symbol `{}` (lines {}-{}, ~{delta} more tokens): per-file region cap {} would be exceeded",
                    symbol.qualified_name, covered.start, covered.end, self.cap
                ));
                continue;
            }
            if delta > *remaining {
                diagnostics.selection.omitted_due_to_budget.push(format!(
                    "{location}: not widened to enclosing symbol `{}` (lines {}-{}, ~{delta} more tokens): exceeds remaining context budget {remaining}",
                    symbol.qualified_name, covered.start, covered.end
                ));
                continue;
            }
            candidate
                .evidence_refs
                .push(format!("{ENCLOSING_SYMBOL_REF}:{}", symbol.id));
            candidate.evidence.push(format!(
                "region widened to enclosing symbol `{}` (lines {}-{})",
                symbol.qualified_name, covered.start, covered.end
            ));
            selected[index] = candidate;
            self.file_tokens = self.file_tokens.saturating_add(delta);
            *remaining = remaining.saturating_sub(delta);
        }
    }

    /// The file's other task-ranked units, in rank order. Selection dropped them for the
    /// per-file cap or the budget; the omission it recorded is withdrawn when one comes back.
    fn readmit_ranked_units(
        &mut self,
        selected: &mut Vec<SearchResult>,
        ranked: &[SearchResult],
        selected_keys: &[RetrievalUnitKey],
        remaining: &mut usize,
        diagnostics: &mut RetrievalDiagnostics,
    ) {
        let mut admitted = Vec::<RetrievalUnitKey>::new();
        for (position, result) in ranked.iter().enumerate() {
            if normalize_path(&result.path) != self.path {
                continue;
            }
            let key = RetrievalUnitKey::from_result(result);
            if selected_keys.contains(&key) || admitted.contains(&key) {
                continue;
            }
            let Some(range) = result.line_range.as_ref() else {
                continue;
            };
            if overlaps_unit(selected, self.path, None, range) {
                continue;
            }
            let tokens = estimate_search_result_tokens(result);
            if self.file_tokens.saturating_add(tokens) > self.cap || tokens > *remaining {
                continue;
            }
            let mut unit = result.clone();
            ensure_evidence_refs(&mut unit);
            unit.evidence_refs
                .push(format!("{RANKED_UNIT_REF}:{}", position + 1));
            unit.evidence.push(format!(
                "same-file unit re-admitted by region widening (task rank {})",
                position + 1
            ));
            withdraw_omission(diagnostics, &unit);
            let insert_at = unit_indices(selected, self.path)
                .last()
                .map(|index| index + 1)
                .unwrap_or(selected.len());
            selected.insert(insert_at, unit);
            admitted.push(key);
            self.file_tokens = self.file_tokens.saturating_add(tokens);
            *remaining = remaining.saturating_sub(tokens);
        }
    }

    /// Grow each of the file's units into the chunk after it, then the one before it, while
    /// the cap and budget allow. Two units never absorb the same chunk.
    fn absorb_adjacent_chunks(&mut self, selected: &mut [SearchResult], remaining: &mut usize) {
        for index in unit_indices(selected, self.path) {
            loop {
                let mut grew = false;
                for after in [true, false] {
                    let Some(range) = selected[index].line_range.clone() else {
                        break;
                    };
                    let neighbor = self.chunks.iter().find(|chunk| {
                        if after {
                            chunk.range.start == range.end.saturating_add(1)
                        } else {
                            chunk.range.end.saturating_add(1) == range.start
                        }
                    });
                    let Some(chunk) = neighbor else {
                        continue;
                    };
                    if overlaps_unit(selected, self.path, Some(index), &chunk.range) {
                        continue;
                    }
                    let extended = if after {
                        LineRange {
                            start: range.start,
                            end: chunk.range.end,
                        }
                    } else {
                        LineRange {
                            start: chunk.range.start,
                            end: range.end,
                        }
                    };
                    // Rebuild the text from the chunks rather than appending to the unit's
                    // snippet: a lexical stream's snippet is an excerpt of its chunk, and the
                    // widened range must show every line it claims.
                    let Some((covered, snippet)) = region_snippet(self.chunks, &extended) else {
                        continue;
                    };
                    if covered != extended {
                        continue;
                    }
                    let mut candidate = selected[index].clone();
                    let before = estimate_search_result_tokens(&candidate);
                    candidate.line_range = Some(extended);
                    candidate.snippet = snippet;
                    let delta = estimate_search_result_tokens(&candidate).saturating_sub(before);
                    if self.file_tokens.saturating_add(delta) > self.cap || delta > *remaining {
                        continue;
                    }
                    candidate.evidence_refs.push(format!(
                        "{ADJACENT_UNIT_REF}:{}-{}",
                        chunk.range.start, chunk.range.end
                    ));
                    candidate.evidence.push(format!(
                        "region extended to adjacent chunk (lines {}-{})",
                        chunk.range.start, chunk.range.end
                    ));
                    selected[index] = candidate;
                    self.file_tokens = self.file_tokens.saturating_add(delta);
                    *remaining = remaining.saturating_sub(delta);
                    grew = true;
                }
                if !grew {
                    break;
                }
            }
        }
    }
}

fn unit_indices(selected: &[SearchResult], path: &str) -> Vec<usize> {
    selected
        .iter()
        .enumerate()
        .filter(|(_, result)| normalize_path(&result.path) == path)
        .map(|(index, _)| index)
        .collect()
}

fn overlaps_unit(
    selected: &[SearchResult],
    path: &str,
    except: Option<usize>,
    range: &LineRange,
) -> bool {
    unit_indices(selected, path)
        .into_iter()
        .filter(|index| Some(*index) != except)
        .filter_map(|index| selected[index].line_range.as_ref())
        .any(|existing| existing.start <= range.end && range.start <= existing.end)
}

/// The smallest symbol whose range strictly extends the unit's: a method chunk's class, or a
/// class-header chunk's own class body.
fn enclosing_symbol<'a>(
    symbols: &[&'a Symbol],
    range: &LineRange,
) -> Option<(&'a Symbol, LineRange)> {
    symbols
        .iter()
        .filter_map(|symbol| {
            symbol
                .range
                .clone()
                .map(|symbol_range| (*symbol, symbol_range))
        })
        .filter(|(_, symbol_range)| {
            symbol_range.start <= range.start
                && symbol_range.end >= range.end
                && (symbol_range.start < range.start || symbol_range.end > range.end)
        })
        .min_by_key(|(_, symbol_range)| {
            (
                symbol_range.end.saturating_sub(symbol_range.start),
                symbol_range.start,
            )
        })
}

/// The chunk text covering `wanted`, clipped to the lines the chunks actually hold, so the
/// reported range never claims lines the snippet does not show.
fn region_snippet(chunks: &[&CodeChunk], wanted: &LineRange) -> Option<(LineRange, String)> {
    let mut lines = Vec::new();
    let mut covered: Option<LineRange> = None;
    for chunk in chunks {
        if chunk.range.end < wanted.start || chunk.range.start > wanted.end {
            continue;
        }
        // `split` rather than `lines()`: a chunk that ends in a blank line ends in "\n", and
        // `lines()` would drop that line, leaving the rebuilt text one line short of its range.
        for (offset, line) in chunk.text.split('\n').enumerate() {
            let line_no = chunk.range.start.saturating_add(offset as u32);
            if line_no < wanted.start || line_no > wanted.end {
                continue;
            }
            lines.push(line.trim_end_matches('\r'));
            covered = Some(match covered.take() {
                Some(range) => LineRange {
                    start: range.start.min(line_no),
                    end: range.end.max(line_no),
                },
                None => LineRange::single(line_no),
            });
        }
    }
    covered.map(|range| (range, lines.join("\n")))
}

fn ensure_evidence_refs(result: &mut SearchResult) {
    if result.evidence_refs.is_empty() {
        result.evidence_refs = result.derived_evidence_ids();
    }
}

fn retarget_trace(
    diagnostics: &mut RetrievalDiagnostics,
    original: &RetrievalUnitKey,
    current: RetrievalUnitKey,
) {
    for trace in &mut diagnostics.traces {
        if trace.unit_key.as_ref() == Some(original) {
            trace.unit_key = Some(current.clone());
        }
    }
}

/// Selection recorded this unit as omitted (per-file cap or budget, and as high-value evidence
/// when it was). Re-admitting it makes those records false, so withdraw one occurrence each.
fn withdraw_omission(diagnostics: &mut RetrievalDiagnostics, unit: &SearchResult) {
    let path = unit.path.display().to_string();
    let selection = &mut diagnostics.selection;
    let cap_message = format!("{path}: per-file context unit cap ");
    let budget_message = format!(
        "{path}: estimated {} tokens exceeds remaining context budget ",
        estimate_search_result_tokens(unit)
    );
    for (list, prefix) in [
        (&mut selection.omitted_due_to_caps, &cap_message),
        (&mut selection.omitted_due_to_budget, &budget_message),
    ] {
        if let Some(position) = list.iter().position(|message| message.starts_with(prefix)) {
            list.remove(position);
        }
    }
    let location = format!(
        "{path}{}: ",
        unit.line_range
            .as_ref()
            .map(|range| format!(":{}-{}", range.start, range.end))
            .unwrap_or_default()
    );
    let Some(position) = selection
        .omitted_high_value
        .iter()
        .position(|entry| entry.starts_with(&location))
    else {
        return;
    };
    let entry = selection.omitted_high_value.remove(position);
    let caveat = entry[location.len()..].to_string();
    let still_cited = selection
        .omitted_high_value
        .iter()
        .any(|other| other.ends_with(&caveat));
    if still_cited {
        return;
    }
    selection.caveats.retain(|existing| existing != &caveat);
    diagnostics.caveats.retain(|existing| existing != &caveat);
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, RepositoryId, RetrievalAuthority,
        RetrievalTrace, SymbolId, SymbolKind,
    };

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Rust,
            size_bytes: 1_000,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(id: &str, file: &File, start: u32, end: u32) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: id.into(),
            qualified_name: format!("crate::{id}"),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
            range: Some(LineRange { start, end }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        }
    }

    /// Chunks of `lines` lines each, one line of text per source line so ranges stay honest.
    fn chunks(file: &File, spans: &[(u32, u32)]) -> Vec<CodeChunk> {
        spans
            .iter()
            .map(|(start, end)| CodeChunk {
                id: format!("{}:{start}", file.id),
                file_id: file.id.clone(),
                range: LineRange {
                    start: *start,
                    end: *end,
                },
                language: Language::Rust,
                text: (*start..=*end)
                    .map(|line| format!("line {line} of {}", file.path.display()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                symbol_id: None,
            })
            .collect()
    }

    fn unit(path: &str, chunk: &CodeChunk) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: Some(chunk.range.clone()),
            snippet: chunk.text.clone(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.9,
            score_breakdown: Vec::new(),
        }
    }

    fn trace(result: &SearchResult) -> RetrievalTrace {
        RetrievalTrace {
            path: result.path.clone(),
            unit_key: Some(RetrievalUnitKey::from_result(result)),
            fused_score: result.score,
            authority: RetrievalAuthority::Exact,
            contributions: Vec::new(),
        }
    }

    fn budget(region_files: usize, cap: usize) -> ContextBudget {
        ContextBudget {
            region_files,
            region_tokens_per_file: cap,
            ..ContextBudget::from_file_limit(8)
        }
    }

    #[test]
    fn widens_to_enclosing_symbol_and_keeps_trace_attribution() {
        let file = file("a", "src/a.rs");
        let chunks = chunks(&file, &[(1, 4), (5, 9), (10, 14)]);
        let symbols = vec![symbol("outer", &file, 1, 14)];
        let selected = vec![unit("src/a.rs", &chunks[1])];
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![trace(&selected[0])],
            ..Default::default()
        };

        let widened = widen_selected_regions(
            selected,
            &[],
            &[file],
            &chunks,
            &symbols,
            &budget(3, 10_000),
            &mut diagnostics,
        );

        assert_eq!(widened.len(), 1);
        assert_eq!(widened[0].line_range, Some(LineRange { start: 1, end: 14 }));
        assert_eq!(widened[0].snippet.lines().count(), 14);
        assert!(widened[0]
            .evidence_refs
            .iter()
            .any(|reference| reference == "region:enclosing-symbol:outer"));
        // The original retrieval identity is preserved in the evidence ids...
        assert!(widened[0]
            .evidence_refs
            .iter()
            .any(|reference| reference == "search:src/a.rs:5-9:0"));
        // ...and the trace now answers for the widened unit.
        assert!(crate::retrieval_trace_for_result(&diagnostics, &widened[0]).is_some());
    }

    #[test]
    fn per_file_cap_refuses_a_class_sized_enclosing_symbol_and_records_why() {
        let file = file("a", "src/a.rs");
        let chunks = chunks(&file, &[(1, 4), (5, 9), (10, 200)]);
        let symbols = vec![symbol("huge", &file, 1, 200)];
        let selected = vec![unit("src/a.rs", &chunks[1])];
        let mut diagnostics = RetrievalDiagnostics::default();

        let widened = widen_selected_regions(
            selected,
            &[],
            &[file],
            &chunks,
            &symbols,
            &budget(3, 200),
            &mut diagnostics,
        );

        // The class body was refused; the small chunk before the unit still fit under the cap.
        assert_eq!(widened[0].line_range, Some(LineRange { start: 1, end: 9 }));
        assert!(widened[0]
            .evidence_refs
            .iter()
            .all(|reference| !reference.starts_with("region:enclosing")));
        assert!(diagnostics
            .selection
            .omitted_due_to_caps
            .iter()
            .any(|message| message.contains("enclosing symbol `crate::huge`")));
    }

    #[test]
    fn readmits_same_file_ranked_units_and_withdraws_their_omission() {
        let file = file("a", "src/a.rs");
        let chunks = chunks(&file, &[(1, 4), (5, 9), (10, 14), (30, 40)]);
        let first = unit("src/a.rs", &chunks[0]);
        let far = unit("src/a.rs", &chunks[3]);
        let ranked = vec![first.clone(), far.clone()];
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![trace(&first), trace(&far)],
            ..Default::default()
        };
        diagnostics
            .selection
            .omitted_due_to_caps
            .push("src/a.rs: per-file context unit cap 1 reached".into());
        let caveat = "high-value evidence omitted by per-file cap: src/a.rs: per-file context unit cap 1 reached".to_string();
        diagnostics
            .selection
            .omitted_high_value
            .push(format!("src/a.rs:30-40: {caveat}"));
        diagnostics.selection.caveats.push(caveat.clone());
        diagnostics.caveats.push(caveat.clone());

        let widened = widen_selected_regions(
            vec![first],
            &ranked,
            &[file],
            &chunks,
            &[],
            &budget(3, 10_000),
            &mut diagnostics,
        );

        let ranges = widened
            .iter()
            .map(|result| result.line_range.clone().unwrap())
            .collect::<Vec<_>>();
        // Adjacent chunks were absorbed into the first unit; the far unit came back as its own.
        assert_eq!(
            ranges,
            vec![
                LineRange { start: 1, end: 14 },
                LineRange { start: 30, end: 40 }
            ]
        );
        assert!(widened[1]
            .evidence_refs
            .iter()
            .any(|reference| reference == "region:ranked-unit:2"));
        assert!(diagnostics.selection.omitted_due_to_caps.is_empty());
        assert!(diagnostics.selection.omitted_high_value.is_empty());
        assert!(!diagnostics.selection.caveats.contains(&caveat));
        assert!(!diagnostics.caveats.contains(&caveat));
        assert!(widened
            .iter()
            .all(|result| crate::retrieval_trace_for_result(&diagnostics, result).is_some()));
    }

    #[test]
    fn only_the_top_region_files_widen_and_no_other_file_is_displaced() {
        let a = file("a", "src/a.rs");
        let b = file("b", "src/b.rs");
        let a_chunks = chunks(&a, &[(1, 4), (5, 9)]);
        let b_chunks = chunks(&b, &[(1, 4), (5, 9)]);
        let selected = vec![
            unit("src/a.rs", &a_chunks[1]),
            unit("src/b.rs", &b_chunks[1]),
        ];
        let mut diagnostics = RetrievalDiagnostics::default();
        let mut all_chunks = a_chunks.clone();
        all_chunks.extend(b_chunks.clone());

        let widened = widen_selected_regions(
            selected,
            &[],
            &[a, b],
            &all_chunks,
            &[],
            &budget(1, 10_000),
            &mut diagnostics,
        );

        assert_eq!(widened.len(), 2);
        assert_eq!(widened[0].line_range, Some(LineRange { start: 1, end: 9 }));
        assert_eq!(widened[1].path, std::path::PathBuf::from("src/b.rs"));
        assert_eq!(widened[1].line_range, Some(LineRange { start: 5, end: 9 }));
        assert!(widened[1]
            .evidence_refs
            .iter()
            .all(|r| !r.starts_with("region:")));
    }

    #[test]
    fn widening_spends_only_the_budget_selection_left_over() {
        let file = file("a", "src/a.rs");
        let chunks = chunks(&file, &[(1, 4), (5, 9), (10, 14)]);
        let selected = vec![unit("src/a.rs", &chunks[1])];
        let spent = estimate_search_result_tokens(&selected[0]);
        let exhausted = ContextBudget {
            max_tokens: spent,
            reserve_for_instructions: 0,
            reserve_for_validation: 0,
            max_per_file: 2,
            max_primary_files: 4,
            region_files: 3,
            region_tokens_per_file: 10_000,
        };
        let mut diagnostics = RetrievalDiagnostics::default();

        let widened = widen_selected_regions(
            selected,
            &[],
            &[file],
            &chunks,
            &[],
            &exhausted,
            &mut diagnostics,
        );

        assert_eq!(widened[0].line_range, Some(LineRange { start: 5, end: 9 }));
    }

    #[test]
    fn region_steps_reads_tags_in_application_order() {
        let mut result = unit("src/a.rs", &chunks(&file("a", "src/a.rs"), &[(1, 2)])[0]);
        result.evidence_refs = vec![
            "search:src/a.rs:1-2:0".into(),
            "region:enclosing-symbol:x".into(),
            "region:adjacent-unit:3-4".into(),
            "region:adjacent-unit:5-6".into(),
        ];
        assert_eq!(
            region_steps(&result),
            vec![
                "region:enclosing-symbol",
                "region:adjacent-unit",
                "region:adjacent-unit"
            ]
        );
    }
}

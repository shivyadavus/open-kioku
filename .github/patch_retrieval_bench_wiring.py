from pathlib import Path


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    path.write_text(text.replace(old, new, 1))


lib = Path("crates/open-kioku-cli/src/lib.rs")
replace_once(
    lib,
    'include!("types.rs");',
    'include!("bench/retrieval.rs");\ninclude!("types.rs");',
    "retrieval include",
)

types = Path("crates/open-kioku-cli/src/types.rs")
replace_once(
    types,
    '    WorkflowBench(WorkflowBenchArgs),\n    ContractBench(ContractBenchArgs),',
    '    WorkflowBench(WorkflowBenchArgs),\n    RetrievalBench(RetrievalBenchArgs),\n    ContractBench(ContractBenchArgs),',
    "retrieval command variant",
)

commands = Path("crates/open-kioku-cli/src/commands/mod.rs")
retrieval_arm = '''        Command::RetrievalBench(args) => {
            let min_cases = args.min_cases;
            let min_fusion_recall_at_10 = args.min_fusion_recall_at_10;
            let min_fusion_mrr = args.min_fusion_mrr;
            let max_no_gold_false_positive_rate = args.max_no_gold_false_positive_rate;
            let report = run_retrieval_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_retrieval_bench_report(&report);
            }
            if report.case_count < min_cases {
                anyhow::bail!(
                    "retrieval benchmark loaded {} cases, below required {}",
                    report.case_count,
                    min_cases
                );
            }
            let gate = retrieval_gate_quality(&report)?;
            if gate.recall_at_10 < min_fusion_recall_at_10 {
                anyhow::bail!(
                    "retrieval Fusion holdout recall@10 {:.3} is below required {:.3}",
                    gate.recall_at_10,
                    min_fusion_recall_at_10
                );
            }
            if gate.mean_reciprocal_rank < min_fusion_mrr {
                anyhow::bail!(
                    "retrieval Fusion holdout MRR {:.3} is below required {:.3}",
                    gate.mean_reciprocal_rank,
                    min_fusion_mrr
                );
            }
            if gate.no_gold_false_positive_rate > max_no_gold_false_positive_rate {
                anyhow::bail!(
                    "retrieval Fusion holdout no-gold false-positive rate {:.3} exceeds required {:.3}",
                    gate.no_gold_false_positive_rate,
                    max_no_gold_false_positive_rate
                );
            }
        }
'''
replace_once(
    commands,
    '        Command::ContractBench(args) => {',
    retrieval_arm + '        Command::ContractBench(args) => {',
    "retrieval command handler",
)

retrieval = Path("crates/open-kioku-cli/src/bench/retrieval.rs")
text = retrieval.read_text()
family_impl_marker = '''enum RetrievalTaskFamily {
    IssueToCode,
    CodeToTest,
    TraceToCode,
    CommentToContext,
    EditToRipple,
}
'''
family_impl = family_impl_marker + '''
impl RetrievalTaskFamily {
    fn label(self) -> &'static str {
        match self {
            Self::IssueToCode => "issue_to_code",
            Self::CodeToTest => "code_to_test",
            Self::TraceToCode => "trace_to_code",
            Self::CommentToContext => "comment_to_context",
            Self::EditToRipple => "edit_to_ripple",
        }
    }
}
'''
if text.count(family_impl_marker) != 1:
    raise SystemExit("retrieval task family marker changed")
text = text.replace(family_impl_marker, family_impl, 1)
old_group = '''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            format!("{:?}", case.task_family).to_ascii_lowercase()
        }),'''
new_group = '''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
        }),'''
if text.count(old_group) != 1:
    raise SystemExit("retrieval task family grouping changed")
text = text.replace(old_group, new_group, 1)

old_symbol_key = 'result.symbol.as_deref().unwrap_or("")'
new_symbol_key = '''result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.qualified_name.as_str())
                    .unwrap_or("")'''
if text.count(old_symbol_key) != 1:
    raise SystemExit("retrieval candidate symbol identity block changed")
text = text.replace(old_symbol_key, new_symbol_key, 1)

old_symbol_tokens = 'result.symbol.as_deref().map(str::len).unwrap_or(0)'
new_symbol_tokens = '''result
            .symbol
            .as_ref()
            .map(|symbol| symbol.qualified_name.chars().count())
            .unwrap_or(0)'''
if text.count(old_symbol_tokens) != 1:
    raise SystemExit("retrieval token estimator symbol block changed")
text = text.replace(old_symbol_tokens, new_symbol_tokens, 1)

retrieval.write_text(text)

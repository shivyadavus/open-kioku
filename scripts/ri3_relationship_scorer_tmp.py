from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)


types = Path("crates/open-kioku-cli/src/types.rs")
text = types.read_text()
text = replace_exact(
    text,
    "    ContractBench(ContractBenchArgs),\n",
    "    RelationshipBench(RelationshipBenchArgs),\n    ContractBench(ContractBenchArgs),\n",
    "relationship bench command variant",
)
args_anchor = "#[derive(Args)]\nstruct ContractBenchArgs {\n"
args = '''#[derive(Args)]
struct RelationshipBenchArgs {
    /// Versioned JSON relationship conformance corpus.
    #[arg(long, value_name = "CORPUS_JSON")]
    corpus: PathBuf,

    /// JSON observations produced by a resolver/index run.
    #[arg(long, value_name = "OBSERVATIONS_JSON")]
    observations: PathBuf,

    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,
}

'''
text = replace_exact(text, args_anchor, args + args_anchor, "relationship bench args")
types.write_text(text)

commands = Path("crates/open-kioku-cli/src/commands/mod.rs")
text = commands.read_text()
command_anchor = "        Command::ContractBench(args) => {\n"
command_arm = '''        Command::RelationshipBench(args) => {
            run_relationship_bench_command(args, cli.json)?;
        }
'''
text = replace_exact(
    text,
    command_anchor,
    command_arm + command_anchor,
    "relationship bench command dispatch",
)
commands.write_text(text)

relationship = Path("crates/open-kioku-cli/src/bench/relationship.rs")
text = relationship.read_text()
if "relationship_ratio(" not in text:
    observed = text.count("ratio(")
    if observed < 2:
        raise SystemExit(f"relationship ratio helper seam changed: {observed}")
    text = text.replace("ratio(", "relationship_ratio(")

sort_old = "    relationships.sort_by(|left, right| observed_relationship_key(left).cmp(&observed_relationship_key(right)));\n"
sort_new = "    relationships.sort_by_key(observed_relationship_key);\n"
text = replace_exact(text, sort_old, sort_new, "observed relationship deterministic sort")

key_old = '''fn observed_relationship_key(
    relationship: &RelationshipBenchObservedRelationship,
) -> (String, String, String, u8, Vec<String>, Vec<(u32, u32, u32, u32)>) {
'''
key_new = '''type ObservedRelationshipKey = (
    String,
    String,
    String,
    u8,
    Vec<String>,
    Vec<(u32, u32, u32, u32)>,
);

fn observed_relationship_key(
    relationship: &RelationshipBenchObservedRelationship,
) -> ObservedRelationshipKey {
'''
text = replace_exact(text, key_old, key_new, "observed relationship key type")

validate_anchor = "fn validate_relationship_bench_corpus(corpus: &RelationshipBenchCorpus) -> anyhow::Result<()> {\n"
runner = '''fn run_relationship_bench_command(
    args: RelationshipBenchArgs,
    json: bool,
) -> anyhow::Result<()> {
    let corpus = load_relationship_bench_corpus(&args.corpus)?;
    let raw = fs::read_to_string(&args.observations).with_context(|| {
        format!(
            "failed to read relationship benchmark observations {}",
            args.observations.display()
        )
    })?;
    let observations: Vec<RelationshipBenchObservation> = serde_json::from_str(&raw)
        .with_context(|| {
            format!(
                "invalid relationship benchmark observations {}",
                args.observations.display()
            )
        })?;
    let report = score_relationship_bench(&corpus, &observations)?;
    let rendered = serde_json::to_string_pretty(&report)?;

    if let Some(path) = &args.write {
        fs::write(path, &rendered).with_context(|| {
            format!(
                "failed to write relationship benchmark report {}",
                path.display()
            )
        })?;
    }

    if json {
        println!("{rendered}");
    } else {
        println!(
            "Relationship conformance: {} cases | precision {:.4} | recall {:.4}",
            report.overall.cases, report.overall.precision, report.overall.recall
        );
        println!(
            "MustNotEmit/ambiguous FP rate {:.4} | exact ranges {:.4} | proofs {:.4}",
            report.overall.must_not_emit_false_positive_rate,
            report.overall.exact_range_compliance,
            report.overall.proof_compliance
        );
        println!("Observation digest: {}", report.observation_digest);
        if let Some(path) = &args.write {
            println!("Wrote report to {}", path.display());
        }
        if !report.diagnostics.is_empty() {
            println!("Diagnostics: {}", report.diagnostics.len());
            for diagnostic in report.diagnostics.iter().take(20) {
                println!(
                    "- {} [{}] {}",
                    diagnostic.case_id, diagnostic.kind, diagnostic.message
                );
            }
            if report.diagnostics.len() > 20 {
                println!("- ... {} more", report.diagnostics.len() - 20);
            }
        }
    }
    Ok(())
}

'''
text = replace_exact(
    text,
    validate_anchor,
    runner + validate_anchor,
    "relationship bench command runner",
)
relationship.write_text(text)

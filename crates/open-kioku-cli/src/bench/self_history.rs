// `ok bench self`: context retrieval scored against the repository's own recent commits
// (docs/retrieval-benchmark.md, "Benchmarking a repository against its own history").

/// Primary results the pack is built from; `ok context` builds with the same number. The ranked
/// list a case is scored over is longer: these primary files, then up to ten supporting files.
const BENCH_SELF_CONTEXT_LIMIT: usize = 20;
/// The most files a pack presents, and so the largest rank a case can record.
const BENCH_SELF_MAX_RANKED_FILES: usize = 30;
const BENCH_SELF_REPORT_VERSION: u32 = 2;
/// The `--min-query-words` and `--max-files` defaults of `scripts/commit-derived-cases.py`.
const BENCH_SELF_MIN_QUERY_WORDS: usize = 3;
const BENCH_SELF_MAX_GOLD_FILES: usize = 5;
/// `MIN_FAMILY_CASES` in `scripts/score-context-cases.py`: below it, one case changing outcome
/// moves a family's metric by more than 0.03.
const BENCH_SELF_MIN_FAMILY_CASES: usize = 34;
const BENCH_SELF_TEMP_PREFIX: &str = "ok-bench-self-";
const BENCH_SELF_FAMILY_ASSIGNMENT: &str = "retrieval_diagnostics.routing.task_family";
/// The ranking adjustment `open-kioku-context` adds to a path a commit scope names. A case whose
/// query earns it on a gold file has been handed its own answer, so the report says when it fired.
const BENCH_SELF_SCOPE_BOOST_SIGNAL: &str = "commit_scope_path_boost";
const BENCH_SELF_REPOSITORY_DOMAIN: &str = "open-kioku bench self repository v1";
const BENCH_SELF_HEAD_DOMAIN: &str = "open-kioku bench self head v1";
const BENCH_SELF_CASE_SET_DOMAIN: &str = "open-kioku bench self case set v1";
const BENCH_SELF_CASE_DOMAIN: &str = "open-kioku bench self case v1";

/// `TaskFamily` in declaration order, which orders the per-family report as the scorer does.
const BENCH_SELF_TASK_FAMILIES: [open_kioku_core::TaskFamily; 8] = [
    open_kioku_core::TaskFamily::IssueToCode,
    open_kioku_core::TaskFamily::CodeToTest,
    open_kioku_core::TaskFamily::TraceToCode,
    open_kioku_core::TaskFamily::CommentToContext,
    open_kioku_core::TaskFamily::EditToRipple,
    open_kioku_core::TaskFamily::Documentation,
    open_kioku_core::TaskFamily::MixedCodeDocs,
    open_kioku_core::TaskFamily::General,
];

fn bench_self_family_name(family: open_kioku_core::TaskFamily) -> &'static str {
    use open_kioku_core::TaskFamily;
    match family {
        TaskFamily::IssueToCode => "issue_to_code",
        TaskFamily::CodeToTest => "code_to_test",
        TaskFamily::TraceToCode => "trace_to_code",
        TaskFamily::CommentToContext => "comment_to_context",
        TaskFamily::EditToRipple => "edit_to_ripple",
        TaskFamily::Documentation => "documentation",
        TaskFamily::MixedCodeDocs => "mixed_code_docs",
        TaskFamily::General => "general",
    }
}

/// A domain-separated SHA-256 over `parts`, so a digest from one field can never equal a digest
/// of the same bytes in another.
fn bench_self_digest(domain: &str, parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0u8]);
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(b"\n");
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// How a case is named in the report. A commit id belongs to the user, so under redaction a case
/// carries a digest of it instead: two reports can still be joined case by case, which comparing
/// by row position cannot do once selection shifts by one commit.
fn bench_self_case_id(sha: &str, reveal_paths: bool) -> String {
    if reveal_paths {
        return sha.to_owned();
    }
    let digest = bench_self_digest(BENCH_SELF_CASE_DOMAIN, &[sha.to_owned()]);
    digest.chars().take("sha256:".len() + 16).collect()
}

#[derive(Debug, Clone, Serialize)]
struct BenchSelfReport {
    report_version: u32,
    open_kioku_version: &'static str,
    paths_redacted: bool,
    requested_commits: usize,
    context_limit: usize,
    max_ranked_files: usize,
    network: &'static str,
    repository: BenchSelfRepositoryReport,
    case_set_digest: String,
    configuration: BenchSelfConfigurationReport,
    selection: BenchSelfSelectionReport,
    cases_scored: usize,
    cases_gold_not_indexed: usize,
    cases_errored: usize,
    cases_with_scope_boost_on_gold: usize,
    gate: BenchSelfGate,
    metrics: Option<BenchSelfMetrics>,
    /// The same metrics with every case whose modified file was absent from the base index
    /// counted as a miss, so a coverage regression lowers the score instead of shrinking the
    /// sample it is computed over.
    metrics_coverage_adjusted: Option<BenchSelfMetrics>,
    by_task_family: BenchSelfFamilySection,
    cases: Vec<BenchSelfCaseReport>,
    caveats: Vec<String>,
}

/// What the numbers were measured on. The commit ids are the user's, so by default only digests
/// are recorded; they are enough to tell whether two reports describe the same base and cases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BenchSelfRepositoryReport {
    digest: String,
    head_digest: String,
    head: Option<String>,
    rev: Option<String>,
}

/// The settings behind the numbers. `ok.toml` is git-ignored, so without this the configuration a
/// report was produced under is in neither the artifact nor version control.
#[derive(Debug, Clone, Serialize)]
struct BenchSelfConfigurationReport {
    ranking: open_kioku_config::RankingConfig,
    history: open_kioku_config::HistoryConfig,
    deny_network: bool,
    scip_enabled: bool,
    semantic_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct BenchSelfSelectionReport {
    scanned_commits: usize,
    selected_commits: usize,
    /// Whether the walk started at the repository's current HEAD. When it did, any commit on the
    /// working branch is a case, so a feature branch measures itself as well as the repository.
    walked_repository_head: bool,
    skipped: BenchSelfSkipCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct BenchSelfSkipCounts {
    root_commit: usize,
    no_modified_source_file: usize,
    too_many_source_files: usize,
    path_in_subject: usize,
    /// Skipped because a conventional-commit scope named a segment of a modified path. Counted
    /// apart from the extractor's own rule: this is the rule this command adds, and how much
    /// corpus it costs on a repository with scoped subjects has to be visible before anyone
    /// reaches for a lever to make a run report.
    scope_names_modified_path: usize,
    short_subject: usize,
    repeated_subject: usize,
}

/// Why a metric set is `null`, or that it is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BenchSelfGate {
    min_cases: usize,
    selected_commits: usize,
    scored_cases: usize,
    coverage_adjusted_cases: usize,
    metrics_suppressed: Option<String>,
    coverage_adjusted_suppressed: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
struct BenchSelfMetrics {
    #[serde(rename = "R@5")]
    recall_at_5: f64,
    #[serde(rename = "R@20")]
    recall_at_20: f64,
    #[serde(rename = "MRR")]
    mrr: f64,
    /// The scorer's name for the share of a case's modified files the pack returned anywhere.
    #[serde(rename = "gold_recall@20")]
    gold_recall: f64,
    /// The denominator, beside the numbers rather than on another line.
    cases: usize,
}

/// One case's contribution: its best gold rank, and how much of its gold set came back at all.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BenchSelfScore {
    rank: Option<usize>,
    gold_recall: f64,
}

impl BenchSelfScore {
    /// A case the index could not answer: no rank, nothing recalled.
    fn miss() -> Self {
        Self {
            rank: None,
            gold_recall: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct BenchSelfFamilySection {
    assignment: &'static str,
    min_cases: usize,
    families: Vec<BenchSelfFamilyReport>,
}

#[derive(Debug, Clone, Serialize)]
struct BenchSelfFamilyReport {
    family: &'static str,
    cases: usize,
    insufficient: bool,
    metrics: BenchSelfMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BenchSelfCaseStatus {
    Scored,
    GoldNotIndexed,
    Error,
}

#[derive(Debug, Clone, Serialize)]
struct BenchSelfCaseReport {
    /// 0-based, newest selected commit first. A join key only within one report: selecting from a
    /// different HEAD shifts every position. Join two reports on `case_id`.
    position: usize,
    case_id: String,
    /// Withheld unless `--reveal-paths`.
    commit: Option<String>,
    /// Withheld unless `--reveal-paths`.
    query: Option<String>,
    status: BenchSelfCaseStatus,
    task_family: Option<&'static str>,
    /// The best rank among the gold files; `None` when the pack returned none of them.
    rank: Option<usize>,
    /// The share of this case's gold files the pack returned anywhere.
    gold_recall: Option<f64>,
    gold: Vec<BenchSelfGoldRank>,
    returned_files: usize,
    /// Whether the commit-scope path boost fired on a gold file: read from the result's score
    /// breakdown, not inferred from the subject.
    commit_scope_boost_on_gold: bool,
    coverage: Option<BenchSelfCoverage>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BenchSelfGoldRank {
    path: String,
    rank: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BenchSelfCoverage {
    discovered: usize,
    considered: usize,
    indexed: usize,
    excluded_by_policy: usize,
    programming_considered: usize,
    programming_indexed: usize,
    /// `IndexCoverage::headline`, which carries counts and ratios only, never a directory.
    headline: String,
}

impl BenchSelfCoverage {
    fn from_index(coverage: &open_kioku_core::IndexCoverage) -> Self {
        let (programming_considered, programming_indexed) = coverage.programming_totals();
        Self {
            discovered: coverage.discovered,
            considered: coverage.considered(),
            indexed: coverage.indexed,
            excluded_by_policy: coverage.excluded_by_policy(),
            programming_considered,
            programming_indexed,
            headline: coverage.headline(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchSelfLogRecord {
    sha: String,
    parents: Vec<String>,
    subject: String,
    /// Paths git reports as modified (`M`); added, deleted, and renamed files are not gold.
    modified: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BenchSelfCommit {
    sha: String,
    parent: String,
    query: String,
    gold: Vec<PathBuf>,
}

/// One record of `git log --format=%H%x1f%P%x1f%s --name-status`, without its NUL separator.
fn parse_bench_self_log_record(record: &str) -> Option<BenchSelfLogRecord> {
    let (header, files) = record.split_once('\n').unwrap_or((record, ""));
    let mut fields = header.splitn(3, '\x1f');
    let sha = fields.next()?.trim();
    if sha.is_empty() {
        return None;
    }
    let parents = fields
        .next()?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let subject = fields.next().unwrap_or_default().to_owned();
    let modified = files
        .lines()
        .filter_map(|line| {
            let (status, path) = line.split_once('\t')?;
            // git quotes a path holding a tab, newline, quote, or backslash; such a path is
            // not unescaped here and so is never gold.
            (status == "M" && !path.is_empty() && !path.starts_with('"'))
                .then(|| PathBuf::from(path))
        })
        .collect();
    Some(BenchSelfLogRecord {
        sha: sha.to_owned(),
        parents,
        subject,
        modified,
    })
}

/// The commit-derived extractor's rules over history read newest first.
#[derive(Debug)]
struct BenchSelfSelector {
    wanted: usize,
    scanned: usize,
    skipped: BenchSelfSkipCounts,
    seen_subjects: BTreeSet<String>,
    selected: Vec<BenchSelfCommit>,
}

impl BenchSelfSelector {
    fn new(wanted: usize) -> Self {
        Self {
            wanted,
            scanned: 0,
            skipped: BenchSelfSkipCounts::default(),
            seen_subjects: BTreeSet::new(),
            selected: Vec::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.selected.len() >= self.wanted
    }

    fn offer(&mut self, record: BenchSelfLogRecord) {
        self.scanned += 1;
        // `--no-merges` leaves at most one parent.
        let Some(parent) = record.parents.first() else {
            self.skipped.root_commit += 1;
            return;
        };
        let gold = record
            .modified
            .iter()
            .filter(|path| open_kioku_languages::detect_language(path.as_path()).is_programming())
            .cloned()
            .collect::<Vec<_>>();
        if gold.is_empty() {
            self.skipped.no_modified_source_file += 1;
            return;
        }
        if gold.len() > BENCH_SELF_MAX_GOLD_FILES {
            self.skipped.too_many_source_files += 1;
            return;
        }
        match bench_self_subject_names_path(&record.subject, &record.modified) {
            Some(BenchSelfLeak::PathInSubject) => {
                self.skipped.path_in_subject += 1;
                return;
            }
            Some(BenchSelfLeak::CommitScope) => {
                self.skipped.scope_names_modified_path += 1;
                return;
            }
            None => {}
        }
        let query = bench_self_clean_query(&record.subject);
        if query.split_whitespace().count() < BENCH_SELF_MIN_QUERY_WORDS {
            self.skipped.short_subject += 1;
            return;
        }
        if !self.seen_subjects.insert(bench_self_subject_key(&query)) {
            self.skipped.repeated_subject += 1;
            return;
        }
        self.selected.push(BenchSelfCommit {
            sha: record.sha,
            parent: parent.clone(),
            query,
            gold,
        });
    }

    fn case_set_digest(&self) -> String {
        bench_self_digest(
            BENCH_SELF_CASE_SET_DOMAIN,
            &self
                .selected
                .iter()
                .map(|commit| commit.sha.clone())
                .collect::<Vec<_>>(),
        )
    }
}

/// How a subject hands the ranker its own answer, counted apart so the cost of each rule is
/// visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BenchSelfLeak {
    /// The extractor's own rule: a path-like token, or a modified file's name or stem.
    PathInSubject,
    /// This command's addition: a conventional-commit scope naming a modified path's segment.
    CommitScope,
}

/// Whether the subject hands the ranker its own answer.
///
/// Three ways it can: a path-like token (the extractor's rule); a token equal to a modified
/// file's name or stem; or a conventional-commit scope naming a segment of a modified path, which
/// `path_matches_scope` in `open-kioku-context` rewards with an explicit score adjustment. The
/// token rules are deliberately broader than the ranker's own tokenisation: a filter that tracked
/// it exactly would stop working, silently, the next time scope parsing changes.
fn bench_self_subject_names_path(subject: &str, modified: &[PathBuf]) -> Option<BenchSelfLeak> {
    if subject.split_whitespace().any(|token| token.contains('/')) {
        return Some(BenchSelfLeak::PathInSubject);
    }
    let mut names = BTreeSet::new();
    let mut segments = BTreeSet::new();
    for path in modified {
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            names.insert(name.to_ascii_lowercase());
        }
        for component in path.components() {
            let Some(component) = component.as_os_str().to_str() else {
                continue;
            };
            let stem = component
                .split_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(component);
            if !stem.is_empty() {
                segments.insert(stem.to_ascii_lowercase());
            }
        }
    }
    let file_stems = modified
        .iter()
        .filter_map(|path| path.file_stem().and_then(|stem| stem.to_str()))
        .map(str::to_ascii_lowercase)
        .collect::<BTreeSet<_>>();
    let scope = bench_self_scope_tokens(subject);
    if !scope.is_empty() && scope.iter().all(|token| segments.contains(token)) {
        return Some(BenchSelfLeak::CommitScope);
    }
    let names_a_file = subject
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
        .map(|token| token.trim_matches('.').to_ascii_lowercase())
        .any(|token| !token.is_empty() && (names.contains(&token) || file_stems.contains(&token)));
    names_a_file.then_some(BenchSelfLeak::PathInSubject)
}

/// The scope tokens `commit_scope_tokens` in `open-kioku-context` reads from a subject:
/// `fix(search): ...` and `[search] ...` both yield `search`. Mirrored rather than shared because
/// the function is private to that crate; the filter above stays a superset of it either way.
fn bench_self_scope_tokens(subject: &str) -> Vec<String> {
    const TYPES: &[&str] = &[
        "feat", "fix", "docs", "doc", "chore", "refactor", "test", "tests", "ci", "build", "perf",
        "style", "revert", "deps", "release", "wip", "misc", "cleanup",
    ];
    let first_line = subject.lines().next().unwrap_or_default().trim();
    let scope = if let Some(rest) = first_line.strip_prefix('[') {
        rest.split_once(']').map(|(scope, _)| scope)
    } else if let Some((prefix, _)) = first_line.split_once(':') {
        let prefix = prefix.trim().trim_end_matches('!');
        if prefix.is_empty() || prefix.len() > 64 || prefix.contains(char::is_whitespace) {
            None
        } else if let Some((_, scoped)) = prefix.split_once('(') {
            scoped.strip_suffix(')')
        } else if TYPES.contains(&prefix.to_ascii_lowercase().as_str())
            || prefix.chars().any(|ch| ch.is_ascii_uppercase())
        {
            None
        } else {
            Some(prefix)
        }
    } else {
        None
    };
    scope
        .map(|scope| {
            scope
                .split(|c: char| c == '/' || c == ',' || c.is_whitespace())
                .filter(|token| !token.is_empty())
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
}

/// `clean_query` in `scripts/commit-derived-cases.py`: PR references removed, backticks
/// unwrapped, whitespace collapsed, and ` .:-` trimmed from both ends.
fn bench_self_clean_query(subject: &str) -> String {
    let unquoted = bench_self_unwrap_backticks(&bench_self_strip_pr_refs(subject));
    unquoted
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| matches!(c, ' ' | '.' | ':' | '-'))
        .to_owned()
}

/// Removes `(#123)` and `#123`, in the extractor's alternation order.
fn bench_self_strip_pr_refs(subject: &str) -> String {
    fn digit_run(chars: &[char], start: usize) -> usize {
        chars
            .iter()
            .skip(start)
            .take_while(|c| c.is_ascii_digit())
            .count()
    }

    let chars = subject.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(subject.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '(' && chars.get(index + 1) == Some(&'#') {
            let digits = digit_run(&chars, index + 2);
            if digits > 0 && chars.get(index + 2 + digits) == Some(&')') {
                index += digits + 3;
                continue;
            }
        }
        if chars[index] == '#' {
            let digits = digit_run(&chars, index + 1);
            if digits > 0 {
                index += digits + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn bench_self_unwrap_backticks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&after[..close]);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// A subject repeated up to numbers and case, such as a release bump, is one case.
fn bench_self_subject_key(query: &str) -> String {
    let mut key = String::with_capacity(query.len());
    let mut in_digits = false;
    for c in query.chars() {
        if c.is_ascii_digit() {
            if !in_digits {
                key.push('#');
            }
            in_digits = true;
        } else {
            in_digits = false;
            key.extend(c.to_lowercase());
        }
    }
    key.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// git for `ok bench self`. Transports are disabled, so a partial clone missing an object fails
/// instead of fetching it, and with `hooks` a checkout runs none of the repository's hooks.
fn bench_self_git(repo: &Path, hooks: Option<&Path>) -> ProcessCommand {
    let mut command = ProcessCommand::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["-c", "protocol.allow=never", "-c", "core.quotePath=false"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .stdin(std::process::Stdio::null());
    if let Some(hooks) = hooks {
        command
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks.display()));
    }
    command
}

fn bench_self_repository_root(repo: &Path) -> anyhow::Result<PathBuf> {
    let repo = repo
        .canonicalize()
        .with_context(|| format!("repository {} does not exist", repo.display()))?;
    let output = bench_self_git(&repo, None)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        anyhow::bail!(
            "{} is not a git repository; `ok bench self` needs its history",
            repo.display()
        );
    }
    let top = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        .canonicalize()
        .context("git reported a repository root that does not exist")?;
    if top != repo {
        anyhow::bail!(
            "run `ok bench self` at the repository root ({}): each base checkout is the whole repository",
            top.display()
        );
    }
    Ok(repo)
}

/// The commit `rev` names, so a run is pinned to one base rather than to whatever HEAD was.
fn bench_self_resolve_commit(repo: &Path, rev: &str) -> anyhow::Result<String> {
    let output = bench_self_git(repo, None)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{rev}^{{commit}}"))
        .output()
        .context("could not run git rev-parse")?;
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() || resolved.is_empty() {
        anyhow::bail!("`{rev}` does not name a commit in this repository");
    }
    Ok(resolved)
}

/// A repository identity that names nothing: a digest over the root commits reachable from `rev`.
fn bench_self_repository_digest(repo: &Path, rev: &str) -> anyhow::Result<String> {
    let output = bench_self_git(repo, None)
        .args(["rev-list", "--max-parents=0"])
        .arg(rev)
        .output()
        .context("could not run git rev-list")?;
    if !output.status.success() {
        anyhow::bail!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut roots = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    roots.sort();
    Ok(bench_self_digest(BENCH_SELF_REPOSITORY_DOMAIN, &roots))
}

fn bench_self_select_commits(
    repo: &Path,
    rev: &str,
    wanted: usize,
) -> anyhow::Result<BenchSelfSelector> {
    use std::io::BufRead;

    let mut child = bench_self_git(repo, None)
        .args([
            "log",
            "--no-merges",
            "--no-renames",
            "--no-color",
            "--no-show-signature",
            "--encoding=UTF-8",
            "--name-status",
            "--format=%x00%H%x1f%P%x1f%s",
        ])
        .arg(rev)
        .arg("--")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not run git log")?;
    let stdout = child.stdout.take().context("git log has no output pipe")?;
    let mut reader = std::io::BufReader::new(stdout);
    let mut selector = BenchSelfSelector::new(wanted);
    let mut record = Vec::new();
    while !selector.is_full() {
        record.clear();
        if reader.read_until(0, &mut record)? == 0 {
            break;
        }
        if record.last() == Some(&0) {
            record.pop();
        }
        if let Some(parsed) = parse_bench_self_log_record(&String::from_utf8_lossy(&record)) {
            selector.offer(parsed);
        }
    }
    if selector.is_full() {
        // The rest of the history is not needed; stopping git keeps a long history cheap.
        let _ = child.kill();
        let _ = child.wait();
        return Ok(selector);
    }
    drop(reader);
    let output = child.wait_with_output().context("git log did not finish")?;
    if !output.status.success() {
        anyhow::bail!(
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(selector)
}

/// Every path a run has created, so an interrupt, an error, or a panic can remove them.
#[derive(Debug, Default)]
struct BenchSelfCleanup {
    repo: Option<PathBuf>,
    temp_root: Option<PathBuf>,
    active_worktree: Option<PathBuf>,
    interrupted: bool,
}

impl BenchSelfCleanup {
    fn release_worktree(&mut self) -> anyhow::Result<()> {
        let Some(worktree) = self.active_worktree.take() else {
            return Ok(());
        };
        let Some(repo) = self.repo.clone() else {
            return Ok(());
        };
        bench_self_remove_worktree(&repo, &worktree)
    }

    /// Removes the active checkout, then the temporary directory, attempting both.
    fn remove_all(&mut self) -> anyhow::Result<()> {
        let released = self.release_worktree();
        let removed = match self.temp_root.take() {
            Some(root) if root.exists() => fs::remove_dir_all(&root).with_context(|| {
                format!(
                    "could not remove the temporary directory {}",
                    root.display()
                )
            }),
            _ => Ok(()),
        };
        released.and(removed)
    }

    fn interrupt(&mut self) -> anyhow::Result<()> {
        self.interrupted = true;
        self.remove_all()
    }
}

fn bench_self_lock(
    cleanup: &Mutex<BenchSelfCleanup>,
) -> std::sync::MutexGuard<'_, BenchSelfCleanup> {
    // A panic while the lock was held leaves the recorded paths intact, and cleanup needs them.
    cleanup
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Removes what a run still holds when it ends early, by an error return or a panic; a run that
/// ends normally has already removed both, so this finds nothing to do.
struct BenchSelfCleanupGuard<'a>(&'a Mutex<BenchSelfCleanup>);

impl Drop for BenchSelfCleanupGuard<'_> {
    fn drop(&mut self) {
        if let Err(err) = bench_self_lock(self.0).remove_all() {
            eprintln!("bench self: {err:#}");
        }
    }
}

fn bench_self_create_temp_root(repo: &Path) -> anyhow::Result<PathBuf> {
    let base = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let root = base.join(format!(
        "{BENCH_SELF_TEMP_PREFIX}{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&base).with_context(|| format!("could not create {}", base.display()))?;
    fs::create_dir(&root).with_context(|| format!("could not create {}", root.display()))?;
    let root = root
        .canonicalize()
        .with_context(|| format!("could not resolve {}", root.display()))?;
    // A temporary directory inside the repository would put every base checkout, and its index,
    // into the tree being benchmarked.
    if root.starts_with(repo) {
        let _ = fs::remove_dir(&root);
        anyhow::bail!(
            "the temporary directory {} is inside the repository; point TMPDIR outside it",
            root.display()
        );
    }
    Ok(root)
}

fn bench_self_checkout(
    cleanup: &Mutex<BenchSelfCleanup>,
    repo: &Path,
    hooks: &Path,
    worktree: &Path,
    revision: &str,
) -> anyhow::Result<()> {
    // Held across `git worktree add`, so an interrupt finds either no checkout or a finished one.
    let mut state = bench_self_lock(cleanup);
    if state.interrupted {
        anyhow::bail!("interrupted before the checkout");
    }
    state.repo = Some(repo.to_path_buf());
    state.active_worktree = Some(worktree.to_path_buf());
    let output = bench_self_git(repo, Some(hooks))
        .args(["worktree", "add", "--detach", "--quiet"])
        .arg(worktree)
        .arg(revision)
        .output()
        .context("could not run git worktree add")?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn bench_self_worktree_registered(repo: &Path, worktree: &Path) -> anyhow::Result<bool> {
    let output = bench_self_git(repo, None)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("could not run git worktree list")?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| Path::new(listed) == worktree))
}

fn bench_self_remove_worktree(repo: &Path, worktree: &Path) -> anyhow::Result<()> {
    if bench_self_worktree_registered(repo, worktree)? {
        // The checkout holds its untracked index under `.ok/`, hence `--force`. A failure is
        // not final: the directory and the registration are removed below.
        let _ = bench_self_git(repo, None)
            .args(["worktree", "remove", "--force"])
            .arg(worktree)
            .output();
    }
    if worktree.exists() {
        fs::remove_dir_all(worktree).with_context(|| {
            format!(
                "could not remove the temporary checkout {}",
                worktree.display()
            )
        })?;
    }
    if bench_self_worktree_registered(repo, worktree)? {
        // Reached when the directory was already gone, which `git worktree remove` refuses.
        // Only this checkout's own administrative entry is deleted: `git worktree prune` would
        // also delete every other stale registration in the user's repository.
        bench_self_remove_administrative_entry(repo, worktree)?;
        if bench_self_worktree_registered(repo, worktree)? {
            anyhow::bail!(
                "git still lists the temporary checkout {}",
                worktree.display()
            );
        }
    }
    Ok(())
}

/// Deletes the one `worktrees/<name>` entry of the repository's common git directory whose
/// `gitdir` file points at `worktree`, and refuses when none or several do.
fn bench_self_remove_administrative_entry(repo: &Path, worktree: &Path) -> anyhow::Result<()> {
    let output = bench_self_git(repo, None)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("could not run git rev-parse")?;
    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    // `git -C <repo>` reports a relative common directory relative to the repository.
    let common = if common.is_absolute() {
        common
    } else {
        repo.join(common)
    };
    let entries = common.join("worktrees");
    let expected = bench_self_normalize_lexically(&worktree.join(".git"));
    let mut matching = Vec::new();
    for entry in
        fs::read_dir(&entries).with_context(|| format!("could not read {}", entries.display()))?
    {
        let admin = entry?.path();
        let Ok(gitdir) = fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let pointed = Path::new(gitdir.trim());
        // git writes an absolute path unless `worktree.useRelativePaths` is set, in which case
        // the path is relative to the entry itself.
        let pointed = if pointed.is_absolute() {
            bench_self_normalize_lexically(pointed)
        } else {
            bench_self_normalize_lexically(&admin.join(pointed))
        };
        if pointed == expected {
            matching.push(admin);
        }
    }
    match matching.as_slice() {
        [admin] => fs::remove_dir_all(admin).with_context(|| {
            format!(
                "could not remove the administrative entry {} of the temporary checkout",
                admin.display()
            )
        }),
        [] => anyhow::bail!(
            "no administrative entry under {} points at the temporary checkout {}",
            entries.display(),
            worktree.display()
        ),
        _ => anyhow::bail!(
            "{} administrative entries under {} point at the temporary checkout {}; none was removed",
            matching.len(),
            entries.display(),
            worktree.display()
        ),
    }
}

/// `.` and `..` resolved without touching the file system, for a path whose directory is gone.
fn bench_self_normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

/// Every index, search, and pack call receives the checkout, never the benchmarked repository.
/// The repository is used for `git` and for reading `ok.toml`, and for nothing else.
fn bench_self_base_config(mut config: OkConfig) -> OkConfig {
    config.security.deny_network = true;
    config.scip.enabled = false;
    config.scip.auto_generate = false;
    config.scip.allow_install = false;
    config.semantic.enabled = false;
    config.semantic.external_provider_allowed = false;
    config.runtime.enabled = false;
    config
}

struct BenchSelfRun<'a> {
    repo: &'a Path,
    temp_root: &'a Path,
    hooks: &'a Path,
    config: &'a OkConfig,
    reveal_paths: bool,
    cleanup: &'a Mutex<BenchSelfCleanup>,
}

enum BenchSelfBaseOutcome {
    GoldNotIndexed {
        coverage: Option<BenchSelfCoverage>,
    },
    Scored {
        coverage: Option<BenchSelfCoverage>,
        ranked: Vec<PathBuf>,
        scope_boosted: BTreeSet<PathBuf>,
        family: open_kioku_core::TaskFamily,
    },
}

/// The stage that failed, reported without its message unless paths are revealed.
type BenchSelfStageError = (&'static str, anyhow::Error);

struct BenchSelfReportInputs<'a> {
    requested_commits: usize,
    min_cases: usize,
    reveal_paths: bool,
    repository: BenchSelfRepositoryReport,
    case_set_digest: String,
    configuration: BenchSelfConfigurationReport,
    selector: &'a BenchSelfSelector,
    walked_repository_head: bool,
    cases: Vec<BenchSelfCaseReport>,
}

fn run_bench_self(
    repo: &Path,
    args: &BenchSelfArgs,
    cleanup: &Mutex<BenchSelfCleanup>,
) -> anyhow::Result<BenchSelfReport> {
    let repo = bench_self_repository_root(repo)?;
    let wanted = usize::try_from(args.commits).context("--commits is too large")?;
    let config = bench_self_base_config(OkConfig::load_from_repo(&repo)?);
    // Pinned once: every later git call uses the resolved commit, so a commit landing mid-run
    // cannot change which cases are selected.
    let head = bench_self_resolve_commit(&repo, &args.rev)?;
    let repository = BenchSelfRepositoryReport {
        digest: bench_self_repository_digest(&repo, &head)?,
        head_digest: bench_self_digest(BENCH_SELF_HEAD_DOMAIN, std::slice::from_ref(&head)),
        head: args.reveal_paths.then(|| head.clone()),
        rev: args.reveal_paths.then(|| args.rev.clone()),
    };
    // A branch's own commits are cases when the walk starts at HEAD, and a commit that adds a
    // benchmark usually misses on its own subject, so the artifact says when that happened.
    let walked_repository_head = bench_self_resolve_commit(&repo, "HEAD")
        .map(|repository_head| repository_head == head)
        .unwrap_or(false);
    let selector = bench_self_select_commits(&repo, &head, wanted)?;

    let _guard = BenchSelfCleanupGuard(cleanup);
    let temp_root = {
        let mut state = bench_self_lock(cleanup);
        if state.interrupted {
            anyhow::bail!("interrupted");
        }
        let root = bench_self_create_temp_root(&repo)?;
        state.repo = Some(repo.clone());
        state.temp_root = Some(root.clone());
        root
    };
    let hooks = temp_root.join("hooks");
    fs::create_dir(&hooks).with_context(|| format!("could not create {}", hooks.display()))?;
    let run = BenchSelfRun {
        repo: &repo,
        temp_root: &temp_root,
        hooks: &hooks,
        config: &config,
        reveal_paths: args.reveal_paths,
        cleanup,
    };
    let mut cases = Vec::with_capacity(selector.selected.len());
    for (position, commit) in selector.selected.iter().enumerate() {
        eprintln!(
            "bench self: base {} of {}",
            position + 1,
            selector.selected.len()
        );
        cases.push(bench_self_run_case(&run, position, commit)?);
    }
    bench_self_lock(cleanup).remove_all()?;
    Ok(bench_self_report(BenchSelfReportInputs {
        requested_commits: wanted,
        min_cases: args.min_cases,
        reveal_paths: args.reveal_paths,
        repository,
        case_set_digest: selector.case_set_digest(),
        configuration: BenchSelfConfigurationReport {
            ranking: config.ranking.clone(),
            history: config.history.clone(),
            deny_network: config.security.deny_network,
            scip_enabled: config.scip.enabled,
            semantic_enabled: config.semantic.enabled,
        },
        selector: &selector,
        walked_repository_head,
        cases,
    }))
}

fn bench_self_run_case(
    run: &BenchSelfRun<'_>,
    position: usize,
    commit: &BenchSelfCommit,
) -> anyhow::Result<BenchSelfCaseReport> {
    let worktree = run.temp_root.join(format!("base-{position}"));
    let outcome = bench_self_checkout(run.cleanup, run.repo, run.hooks, &worktree, &commit.parent)
        .map_err(|err| ("checkout", err))
        .and_then(|()| bench_self_score_base(run, &worktree, commit));
    // Removed before the next base is checked out, whatever the outcome. A checkout that cannot
    // be removed stops the run instead of accumulating behind it.
    bench_self_lock(run.cleanup).release_worktree()?;
    Ok(bench_self_case_report(
        position,
        commit,
        outcome,
        run.reveal_paths,
    ))
}

fn bench_self_score_base(
    run: &BenchSelfRun<'_>,
    worktree: &Path,
    commit: &BenchSelfCommit,
) -> Result<BenchSelfBaseOutcome, BenchSelfStageError> {
    let snapshot = index_repo_with_config(worktree, run.config.clone(), IndexMode::Full)
        .map_err(|err| ("index", err))?;
    let coverage = snapshot
        .manifest
        .quality
        .coverage
        .as_ref()
        .map(BenchSelfCoverage::from_index);
    let answerable = {
        let indexed = snapshot
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<BTreeSet<_>>();
        commit
            .gold
            .iter()
            .all(|path| indexed.contains(path.as_path()))
    };
    drop(snapshot);
    if !answerable {
        return Ok(BenchSelfBaseOutcome::GoldNotIndexed { coverage });
    }
    let store = SqliteStore::open_repo_index(worktree)
        .map_err(|err| ("context", anyhow::Error::from(err)))?
        .ok_or_else(|| {
            (
                "context",
                anyhow::anyhow!("the base index was not published"),
            )
        })?;
    let pack = build_context_pack_with_config(
        worktree,
        &store,
        &commit.query,
        BENCH_SELF_CONTEXT_LIMIT,
        run.config,
    )
    .map_err(|err| ("context", err))?;
    let results = pack
        .primary_files
        .iter()
        .chain(&pack.supporting_files)
        .collect::<Vec<_>>();
    let ranked =
        bench_self_rank_order(results.iter().map(|result| result.path.as_path()), worktree);
    // Read from the score breakdown the ranker wrote, so the report states that the boost did not
    // fire rather than assuming the subject filter kept it from firing.
    let scope_boosted = results
        .iter()
        .filter(|result| {
            result
                .score_breakdown
                .iter()
                .any(|component| component.signal == BENCH_SELF_SCOPE_BOOST_SIGNAL)
        })
        .map(|result| bench_self_relative_path(result.path.as_path(), worktree))
        .collect::<BTreeSet<_>>();
    Ok(BenchSelfBaseOutcome::Scored {
        coverage,
        ranked,
        scope_boosted,
        family: pack.retrieval_diagnostics.routing.task_family,
    })
}

fn bench_self_relative_path(path: &Path, worktree: &Path) -> PathBuf {
    path.strip_prefix(worktree).unwrap_or(path).to_path_buf()
}

/// Files in the order the pack presents them, primary files then supporting files, duplicates
/// collapsed: the order `scripts/score-context-cases.py` ranks.
fn bench_self_rank_order<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
    worktree: &Path,
) -> Vec<PathBuf> {
    let mut ranked = Vec::<PathBuf>::new();
    for path in paths {
        let path = bench_self_relative_path(path, worktree);
        if !ranked.contains(&path) {
            ranked.push(path);
        }
    }
    ranked
}

fn bench_self_case_report(
    position: usize,
    commit: &BenchSelfCommit,
    outcome: Result<BenchSelfBaseOutcome, BenchSelfStageError>,
    reveal_paths: bool,
) -> BenchSelfCaseReport {
    let mut case = BenchSelfCaseReport {
        position,
        case_id: bench_self_case_id(&commit.sha, reveal_paths),
        commit: reveal_paths.then(|| commit.sha.clone()),
        query: reveal_paths.then(|| commit.query.clone()),
        status: BenchSelfCaseStatus::Error,
        task_family: None,
        rank: None,
        gold_recall: None,
        gold: commit
            .gold
            .iter()
            .map(|path| BenchSelfGoldRank {
                path: proof_path(path, reveal_paths),
                rank: None,
            })
            .collect(),
        returned_files: 0,
        commit_scope_boost_on_gold: false,
        coverage: None,
        error: None,
    };
    match outcome {
        Err((stage, err)) => {
            // An error message can name a path, a commit, or a subject.
            case.error = Some(if reveal_paths {
                format!("{stage}: {err:#}")
            } else {
                format!("{stage} failed; message withheld without --reveal-paths")
            });
        }
        Ok(BenchSelfBaseOutcome::GoldNotIndexed { coverage }) => {
            case.status = BenchSelfCaseStatus::GoldNotIndexed;
            case.coverage = coverage;
        }
        Ok(BenchSelfBaseOutcome::Scored {
            coverage,
            ranked,
            scope_boosted,
            family,
        }) => {
            for (path, entry) in commit.gold.iter().zip(case.gold.iter_mut()) {
                entry.rank = ranked
                    .iter()
                    .position(|candidate| candidate == path)
                    .map(|index| index + 1);
            }
            let returned = case.gold.iter().filter(|gold| gold.rank.is_some()).count();
            case.status = BenchSelfCaseStatus::Scored;
            case.rank = case.gold.iter().filter_map(|entry| entry.rank).min();
            case.gold_recall = Some(returned as f64 / case.gold.len().max(1) as f64);
            case.task_family = Some(bench_self_family_name(family));
            case.returned_files = ranked.len();
            case.commit_scope_boost_on_gold =
                commit.gold.iter().any(|path| scope_boosted.contains(path));
            case.coverage = coverage;
        }
    }
    case
}

/// Recall@k counts a case whose best gold rank is at most k; MRR averages 1/best rank, and
/// `gold_recall@20` averages the share of a case's gold files the pack returned. A case with no
/// gold file returned contributes 0 to both. `None` for no cases: a ratio over nothing is not a
/// measurement.
fn bench_self_metrics(scores: &[BenchSelfScore]) -> Option<BenchSelfMetrics> {
    if scores.is_empty() {
        return None;
    }
    let cases = scores.len() as f64;
    let recall_at = |k: usize| {
        scores
            .iter()
            .filter(|score| matches!(score.rank, Some(rank) if rank <= k))
            .count() as f64
            / cases
    };
    Some(BenchSelfMetrics {
        recall_at_5: recall_at(5),
        recall_at_20: recall_at(20),
        mrr: scores
            .iter()
            .filter_map(|score| score.rank)
            .map(|rank| 1.0 / rank as f64)
            .sum::<f64>()
            / cases,
        gold_recall: scores.iter().map(|score| score.gold_recall).sum::<f64>() / cases,
        cases: scores.len(),
    })
}

fn bench_self_score_of(case: &BenchSelfCaseReport) -> BenchSelfScore {
    BenchSelfScore {
        rank: case.rank,
        gold_recall: case.gold_recall.unwrap_or(0.0),
    }
}

fn bench_self_scored_scores(cases: &[BenchSelfCaseReport]) -> Vec<BenchSelfScore> {
    cases
        .iter()
        .filter(|case| case.status == BenchSelfCaseStatus::Scored)
        .map(bench_self_score_of)
        .collect()
}

/// Every case the base index could have answered: the scored ones, plus each case whose modified
/// file the index did not hold, counted as a miss. Errors are a tool failure rather than a
/// retrieval miss and are in neither set; the caveats name them.
fn bench_self_coverage_adjusted_scores(cases: &[BenchSelfCaseReport]) -> Vec<BenchSelfScore> {
    cases
        .iter()
        .filter_map(|case| match case.status {
            BenchSelfCaseStatus::Scored => Some(bench_self_score_of(case)),
            BenchSelfCaseStatus::GoldNotIndexed => Some(BenchSelfScore::miss()),
            BenchSelfCaseStatus::Error => None,
        })
        .collect()
}

/// Why a metric set may not be published. The absolute floor guards a small run; the ratio guards
/// a large one, where a fixed floor would let most of the selected commits fall out of the sample
/// and still print a headline. An indexing change that stops indexing a file class fails the
/// second condition, which is the shape this guard exists for.
fn bench_self_suppression(cases: usize, selected: usize, min_cases: usize) -> Option<String> {
    let mut reasons = Vec::new();
    if cases < min_cases {
        reasons.push(format!(
            "{cases} cases below the --min-cases floor of {min_cases}"
        ));
    }
    if cases * 2 < selected {
        reasons.push(format!(
            "{cases} of {selected} selected commits scored, fewer than half"
        ));
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

fn bench_self_family_section(cases: &[BenchSelfCaseReport]) -> BenchSelfFamilySection {
    let families = BENCH_SELF_TASK_FAMILIES
        .iter()
        .filter_map(|family| {
            let name = bench_self_family_name(*family);
            let scores = cases
                .iter()
                .filter(|case| {
                    case.status == BenchSelfCaseStatus::Scored && case.task_family == Some(name)
                })
                .map(bench_self_score_of)
                .collect::<Vec<_>>();
            let metrics = bench_self_metrics(&scores)?;
            Some(BenchSelfFamilyReport {
                family: name,
                cases: scores.len(),
                insufficient: scores.len() < BENCH_SELF_MIN_FAMILY_CASES,
                metrics,
            })
        })
        .collect();
    BenchSelfFamilySection {
        assignment: BENCH_SELF_FAMILY_ASSIGNMENT,
        min_cases: BENCH_SELF_MIN_FAMILY_CASES,
        families,
    }
}

/// The caveats a reader needs to quote a number correctly, in the artifact rather than in the
/// documentation: several of them depend on the run and cannot be written down in advance.
fn bench_self_caveats(
    metrics: Option<&BenchSelfMetrics>,
    gate: &BenchSelfGate,
    cases_errored: usize,
    cases_with_scope_boost_on_gold: usize,
    paths_redacted: bool,
    walked_repository_head: bool,
) -> Vec<String> {
    let mut caveats = vec![
        "each case indexes the commit's parent in a temporary checkout and ranks the files the commit modified in the context pack this build returns for the commit subject".to_owned(),
        "queries are commit subjects, not issue text, and the cases are this repository's recent commits: the numbers describe this repository at these commits with this build, and are comparable neither with the frozen commit-derived baselines nor with published benchmarks".to_owned(),
        "the report records the Open Kioku version, not the commit the binary was built from; state that commit beside any published number".to_owned(),
    ];
    match metrics {
        Some(metrics) => caveats.push(format!(
            "at {} scored cases one case changing outcome moves R@k by {:.3}, and no confidence interval is computed: read a difference smaller than that as noise",
            metrics.cases,
            1.0 / metrics.cases.max(1) as f64
        )),
        None => caveats.push(format!(
            "no metrics were published: {}",
            gate.metrics_suppressed
                .as_deref()
                .unwrap_or("no case scored")
        )),
    }
    caveats.push(
        "`metrics_coverage_adjusted` counts every case whose modified file was absent from the base index as a miss, so an indexing change that drops a file class lowers it instead of shrinking the sample `metrics` is computed over".to_owned(),
    );
    caveats.push(format!(
        "R@20 cuts at rank 20, while MRR and gold_recall@20 range over the whole returned list, which reaches {BENCH_SELF_MAX_RANKED_FILES} files ({BENCH_SELF_CONTEXT_LIMIT} primary and up to ten supporting); the names follow scripts/score-context-cases.py"
    ));
    caveats.push(
        "queries are commit subjects, and the ranker's `subject_twin_votes` history signal keys on commit subjects, so this corpus's query distribution is that signal's own input distribution".to_owned(),
    );
    caveats.push(
        "task families are the router's labels (retrieval_diagnostics.routing.task_family); a per-family number measures the retrieval policy on the cases routed to it, not whether routing chose the right family".to_owned(),
    );
    caveats.push(format!(
        "a family with fewer than {BENCH_SELF_MIN_FAMILY_CASES} scored cases is marked insufficient and is not printed in the text output; the JSON carries its numbers in full"
    ));
    if walked_repository_head {
        caveats.push(
            "the walk started at the repository's current HEAD, so commits on the working branch are cases and the run measures that branch as well as the repository; pin `--rev` to a merge base or to the main branch's tip to measure the repository alone".to_owned(),
        );
    }
    if cases_errored > 0 {
        caveats.push(format!(
            "{cases_errored} cases failed to run and are in neither metric set: a tool failure is not a retrieval miss, but a run with errors measured less than it selected"
        ));
    }
    if cases_with_scope_boost_on_gold > 0 {
        caveats.push(format!(
            "{cases_with_scope_boost_on_gold} scored cases had the commit-scope path boost fire on a modified file, so the query named the path the ranker rewards and those ranks are optimistic"
        ));
    }
    if paths_redacted {
        caveats.push(
            "paths, commit ids and subjects are redacted; cases are identified by `case_id`, which is a digest of the commit id, and two reports are compared by joining on it rather than on `position`".to_owned(),
        );
    }
    caveats.push(
        "semantic retrieval, SCIP, and state kept under the repository's .ok directory (an activated abstention policy, repository memory) are not used, so a repository that enables them can see different packs from `ok context`".to_owned(),
    );
    caveats
}

fn bench_self_report(inputs: BenchSelfReportInputs<'_>) -> BenchSelfReport {
    let BenchSelfReportInputs {
        requested_commits,
        min_cases,
        reveal_paths,
        repository,
        case_set_digest,
        configuration,
        selector,
        walked_repository_head,
        cases,
    } = inputs;
    let count =
        |status: BenchSelfCaseStatus| cases.iter().filter(|case| case.status == status).count();
    let selected = selector.selected.len();
    let scored = bench_self_scored_scores(&cases);
    let adjusted = bench_self_coverage_adjusted_scores(&cases);
    let gate = BenchSelfGate {
        min_cases,
        selected_commits: selected,
        scored_cases: scored.len(),
        coverage_adjusted_cases: adjusted.len(),
        metrics_suppressed: bench_self_suppression(scored.len(), selected, min_cases),
        coverage_adjusted_suppressed: bench_self_suppression(adjusted.len(), selected, min_cases),
    };
    let metrics = gate
        .metrics_suppressed
        .is_none()
        .then(|| bench_self_metrics(&scored))
        .flatten();
    let metrics_coverage_adjusted = gate
        .coverage_adjusted_suppressed
        .is_none()
        .then(|| bench_self_metrics(&adjusted))
        .flatten();
    let cases_errored = count(BenchSelfCaseStatus::Error);
    let cases_with_scope_boost_on_gold = cases
        .iter()
        .filter(|case| case.commit_scope_boost_on_gold)
        .count();
    let caveats = bench_self_caveats(
        metrics.as_ref(),
        &gate,
        cases_errored,
        cases_with_scope_boost_on_gold,
        !reveal_paths,
        walked_repository_head,
    );
    BenchSelfReport {
        report_version: BENCH_SELF_REPORT_VERSION,
        open_kioku_version: env!("CARGO_PKG_VERSION"),
        paths_redacted: !reveal_paths,
        requested_commits,
        context_limit: BENCH_SELF_CONTEXT_LIMIT,
        max_ranked_files: BENCH_SELF_MAX_RANKED_FILES,
        network: "denied",
        repository,
        case_set_digest,
        configuration,
        selection: BenchSelfSelectionReport {
            scanned_commits: selector.scanned,
            selected_commits: selected,
            walked_repository_head,
            skipped: selector.skipped.clone(),
        },
        cases_scored: scored.len(),
        cases_gold_not_indexed: count(BenchSelfCaseStatus::GoldNotIndexed),
        cases_errored,
        cases_with_scope_boost_on_gold,
        gate,
        metrics,
        metrics_coverage_adjusted,
        by_task_family: bench_self_family_section(&cases),
        cases,
        caveats,
    }
}

/// Runs the benchmark on a blocking thread and removes its checkout and temporary directory on
/// SIGINT or SIGTERM before exiting with 130.
async fn run_bench_self_until_interrupted(
    repo: PathBuf,
    args: BenchSelfArgs,
) -> anyhow::Result<BenchSelfReport> {
    let cleanup = Arc::new(Mutex::new(BenchSelfCleanup::default()));
    let worker_cleanup = Arc::clone(&cleanup);
    let worker = tokio::task::spawn_blocking(move || run_bench_self(&repo, &args, &worker_cleanup));
    tokio::select! {
        joined = worker => joined.map_err(|err| anyhow::anyhow!("bench self stopped unexpectedly: {err}"))?,
        () = bench_self_interrupt_signal() => {
            match bench_self_lock(&cleanup).interrupt() {
                Ok(()) => eprintln!("bench self: interrupted; temporary checkout and index removed"),
                Err(err) => eprintln!("bench self: interrupted; {err:#}"),
            }
            // The indexing thread cannot be cancelled, and the runtime would wait for it on
            // shutdown; its checkout is gone, so the process exits here.
            std::process::exit(130);
        }
    }
}

async fn bench_self_interrupt_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    () = bench_self_ctrl_c() => {}
                    Some(()) = terminate.recv() => {}
                }
            }
            Err(_) => bench_self_ctrl_c().await,
        }
    }
    #[cfg(not(unix))]
    bench_self_ctrl_c().await;
}

async fn bench_self_ctrl_c() {
    // A handler that cannot be installed must never read as an interrupt.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Decimals a sample of this size supports: at 20 cases the resolution is 0.05, so four decimals
/// would state a precision the corpus does not have.
fn bench_self_decimals(cases: usize) -> usize {
    match cases {
        0..=99 => 2,
        100..=999 => 3,
        _ => 4,
    }
}

fn print_bench_self_report(report: &BenchSelfReport) {
    println!(
        "Bench self: {} of {} selected commits scored ({} commits scanned, {} requested)",
        report.cases_scored,
        report.selection.selected_commits,
        report.selection.scanned_commits,
        report.requested_commits
    );
    println!(
        "  base {} of repository {} (case set {}){}",
        report.repository.head_digest,
        report.repository.digest,
        report.case_set_digest,
        if report.selection.walked_repository_head {
            ", walked from the working HEAD"
        } else {
            ", pinned with --rev"
        }
    );
    let skipped = &report.selection.skipped;
    println!(
        "  skipped commits: {} root, {} without a modified source file, {} with more than {} source files, {} naming a path in the subject, {} whose commit scope named a modified path, {} with fewer than {} query words, {} repeating a subject",
        skipped.root_commit,
        skipped.no_modified_source_file,
        skipped.too_many_source_files,
        BENCH_SELF_MAX_GOLD_FILES,
        skipped.path_in_subject,
        skipped.scope_names_modified_path,
        skipped.short_subject,
        BENCH_SELF_MIN_QUERY_WORDS,
        skipped.repeated_subject
    );
    if report.cases_gold_not_indexed > 0 || report.cases_errored > 0 {
        println!(
            "  not scored: {} with a modified file absent from the base index, {} errors",
            report.cases_gold_not_indexed, report.cases_errored
        );
    }
    // The sample travels on the same line as the numbers: a metrics line that can be quoted
    // without its denominator is how a shrinking corpus reads as an improving score.
    match &report.metrics {
        Some(metrics) => {
            let decimals = bench_self_decimals(metrics.cases);
            println!(
                "  over {} scored cases: R@5 {:.*}  R@20 {:.*}  MRR {:.*}  gold_recall@20 {:.*}  (one case moves R@k by {:.3}; no interval)",
                metrics.cases,
                decimals,
                metrics.recall_at_5,
                decimals,
                metrics.recall_at_20,
                decimals,
                metrics.mrr,
                decimals,
                metrics.gold_recall,
                1.0 / metrics.cases.max(1) as f64
            );
        }
        None => println!(
            "  metrics not published: {}",
            report
                .gate
                .metrics_suppressed
                .as_deref()
                .unwrap_or("no case scored")
        ),
    }
    match &report.metrics_coverage_adjusted {
        Some(metrics) => {
            let decimals = bench_self_decimals(metrics.cases);
            println!(
                "  over {} answerable cases, unindexed counted as a miss: R@5 {:.*}  R@20 {:.*}  MRR {:.*}  gold_recall@20 {:.*}",
                metrics.cases,
                decimals,
                metrics.recall_at_5,
                decimals,
                metrics.recall_at_20,
                decimals,
                metrics.mrr,
                decimals,
                metrics.gold_recall
            );
        }
        None => println!(
            "  coverage-adjusted metrics not published: {}",
            report
                .gate
                .coverage_adjusted_suppressed
                .as_deref()
                .unwrap_or("no case scored")
        ),
    }
    println!(
        "  per routed task family ({}):",
        report.by_task_family.assignment
    );
    for family in &report.by_task_family.families {
        if family.insufficient {
            println!(
                "    {:<20} n={}, not reported (fewer than {}; the JSON carries the numbers)",
                family.family, family.cases, report.by_task_family.min_cases
            );
            continue;
        }
        let decimals = bench_self_decimals(family.cases);
        println!(
            "    {:<20} {:>4} cases  R@5 {:.*}  R@20 {:.*}  MRR {:.*}  gold_recall@20 {:.*}",
            family.family,
            family.cases,
            decimals,
            family.metrics.recall_at_5,
            decimals,
            family.metrics.recall_at_20,
            decimals,
            family.metrics.mrr,
            decimals,
            family.metrics.gold_recall
        );
    }
    println!("  per base:");
    for case in &report.cases {
        let rank = case
            .rank
            .map_or_else(|| "none".to_owned(), |rank| rank.to_string());
        let status = match case.status {
            BenchSelfCaseStatus::Scored => "scored",
            BenchSelfCaseStatus::GoldNotIndexed => "gold not indexed",
            BenchSelfCaseStatus::Error => "error",
        };
        let subject = match &case.query {
            Some(query) => format!(" \"{query}\""),
            None => String::new(),
        };
        println!(
            "    {}{subject}: {status}, family {}, best rank {rank}{}",
            case.case_id,
            case.task_family.unwrap_or("-"),
            if case.commit_scope_boost_on_gold {
                ", commit-scope boost fired on a modified file"
            } else {
                ""
            }
        );
        for gold in &case.gold {
            let rank = gold
                .rank
                .map_or_else(|| "not returned".to_owned(), |rank| format!("rank {rank}"));
            println!("      {} {rank}", gold.path);
        }
        if let Some(coverage) = &case.coverage {
            println!("      coverage: {}", coverage.headline);
        }
        if let Some(error) = &case.error {
            println!("      {error}");
        }
    }
    if report.paths_redacted {
        println!("  paths redacted to top-level directory and extension; commits and subjects withheld (--reveal-paths shows them)");
    }
    for caveat in &report.caveats {
        println!("  note: {caveat}");
    }
}

#[cfg(test)]
mod bench_self_tests {
    use super::*;

    fn record(sha: &str, parents: &[&str], subject: &str, modified: &[&str]) -> BenchSelfLogRecord {
        BenchSelfLogRecord {
            sha: sha.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            subject: subject.to_owned(),
            modified: modified.iter().map(PathBuf::from).collect(),
        }
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=Open Kioku Tests",
                "-c",
                "user.email=tests@open-kioku.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn log_records_keep_only_modified_paths_git_did_not_quote() {
        let parsed = parse_bench_self_log_record(
            "abc\x1fdef\x1fround totals\n\nM\tsrc/billing.rs\nA\tsrc/new.rs\nD\tsrc/old.rs\nM\t\"src/odd\\tname.rs\"\n",
        )
        .unwrap();
        assert_eq!(parsed.sha, "abc");
        assert_eq!(parsed.parents, ["def"]);
        assert_eq!(parsed.subject, "round totals");
        assert_eq!(parsed.modified, [PathBuf::from("src/billing.rs")]);

        let root = parse_bench_self_log_record("abc\x1f\x1finitial import\n").unwrap();
        assert!(root.parents.is_empty());
        assert!(parse_bench_self_log_record("").is_none());
    }

    #[test]
    fn selection_applies_the_commit_derived_extractor_rules_newest_first() {
        let six = [
            "src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs", "src/f.rs",
        ];
        let mut selector = BenchSelfSelector::new(3);
        selector.offer(record(
            "a1",
            &["p1"],
            "round invoice totals to whole cents (#41)",
            &["src/billing.rs", "README.md"],
        ));
        selector.offer(record(
            "a2",
            &["p2"],
            "update the release notes for users",
            &["README.md"],
        ));
        selector.offer(record(
            "a3",
            &["p3"],
            "fix parsing in src/auth.rs",
            &["src/auth.rs"],
        ));
        selector.offer(record(
            "a4",
            &["p4"],
            "fix auth.rs token parsing bug",
            &["src/auth.rs"],
        ));
        selector.offer(record("a5", &["p5"], "fix: typo", &["src/auth.rs"]));
        selector.offer(record(
            "a6",
            &["p6"],
            "sync plugin versions for 5.3.0",
            &["src/version.rs"],
        ));
        selector.offer(record(
            "a7",
            &["p7"],
            "Sync plugin versions for 5.2.0",
            &["src/version.rs"],
        ));
        selector.offer(record("a8", &["p8"], "touch six files at once", &six));
        selector.offer(record(
            "a9",
            &[],
            "initial import of the code",
            &["src/lib.rs"],
        ));

        assert_eq!(
            selector
                .selected
                .iter()
                .map(|commit| commit.sha.as_str())
                .collect::<Vec<_>>(),
            ["a1", "a6"]
        );
        assert_eq!(
            selector.skipped,
            BenchSelfSkipCounts {
                root_commit: 1,
                no_modified_source_file: 1,
                too_many_source_files: 1,
                path_in_subject: 2,
                scope_names_modified_path: 0,
                short_subject: 1,
                repeated_subject: 1,
            }
        );
        assert_eq!(selector.scanned, 9);
        assert_eq!(selector.selected[0].parent, "p1");
        assert_eq!(
            selector.selected[0].query,
            "round invoice totals to whole cents"
        );
        assert_eq!(selector.selected[0].gold, [PathBuf::from("src/billing.rs")]);
        assert!(!selector.is_full());

        selector.offer(record(
            "b1",
            &["q1"],
            "cache resolved lookups between runs",
            &["src/registry_state.rs"],
        ));
        assert!(selector.is_full());
    }

    #[test]
    fn queries_are_cleaned_like_the_commit_derived_extractor() {
        assert_eq!(
            bench_self_clean_query("fix: handle `retry` budget (#123)."),
            "fix: handle retry budget"
        );
        assert_eq!(
            bench_self_clean_query("- merge queue #77 cleanup :"),
            "merge queue cleanup"
        );
        assert_eq!(
            bench_self_clean_query("keep `unpaired backtick"),
            "keep `unpaired backtick"
        );
        assert_eq!(bench_self_clean_query("(#12a) stays"), "(a) stays");
        assert_eq!(
            bench_self_subject_key("Sync  plugin versions for 5.3.10"),
            "sync plugin versions for #.#.#"
        );
    }

    #[test]
    fn a_subject_names_a_path_by_a_slash_token_or_a_modified_file_name() {
        let modified = [PathBuf::from("src/lib.rs")];
        let leak = |subject: &str| bench_self_subject_names_path(subject, &modified);
        assert_eq!(
            leak("feat(cli/bench): add a mode"),
            Some(BenchSelfLeak::PathInSubject)
        );
        assert_eq!(
            leak("fix `lib.rs` parsing."),
            Some(BenchSelfLeak::PathInSubject)
        );
        assert_eq!(
            leak("fix parsing in lib.rs."),
            Some(BenchSelfLeak::PathInSubject)
        );
        assert_eq!(leak("fix library parsing"), None);
        assert_eq!(leak("fix main.rs parsing"), None);
    }

    /// `path_matches_scope` in `open-kioku-context` matches a scope token against a path
    /// segment's stem, so a conventional-commit scope can name the gold file without ever
    /// writing a path. A subject that earns the boost is not a query, it is a hint.
    #[test]
    fn a_conventional_commit_scope_naming_a_modified_path_segment_is_a_leak() {
        // A scope equal to the modified file's stem: counted apart from the extractor's rule,
        // because on a repository of scoped subjects this rule decides how much corpus is left.
        let file = [PathBuf::from("src/search.rs")];
        assert_eq!(
            bench_self_subject_names_path("fix(search): tighten the analyzer", &file),
            Some(BenchSelfLeak::CommitScope)
        );
        let nested = [PathBuf::from("src/search/mod.rs")];
        assert_eq!(
            bench_self_subject_names_path("fix(search): tighten the analyzer", &nested),
            Some(BenchSelfLeak::CommitScope)
        );
        // The scope names no segment of the modified path, so the boost cannot fire.
        let elsewhere = [PathBuf::from("crates/open-kioku-context/src/lib.rs")];
        assert_eq!(
            bench_self_subject_names_path("fix(context): widen the region", &elsewhere),
            None
        );
        // A word that happens to match a directory, with no scope prefix, earns nothing.
        let tests = [PathBuf::from("tests/parser_cases.rs")];
        assert_eq!(
            bench_self_subject_names_path("add tests for the parser", &tests),
            None
        );
    }

    #[test]
    fn scope_tokens_follow_the_ranker_for_parenthesised_bracketed_and_prose_subjects() {
        assert_eq!(bench_self_scope_tokens("fix(search): x"), ["search"]);
        assert_eq!(bench_self_scope_tokens("[search] x"), ["search"]);
        assert_eq!(
            bench_self_scope_tokens("fix(verify,plan): x"),
            ["verify", "plan"]
        );
        assert!(bench_self_scope_tokens("fix: x").is_empty());
        assert!(bench_self_scope_tokens("Note: x").is_empty());
        assert!(bench_self_scope_tokens("no colon here").is_empty());
    }

    #[test]
    fn base_indexes_deny_network_and_disable_scip_semantic_and_runtime_whatever_the_repository_configures(
    ) {
        let mut config = OkConfig::default();
        config.security.deny_network = false;
        config.scip.enabled = true;
        config.scip.auto_generate = true;
        config.scip.allow_install = true;
        config.semantic.enabled = true;
        config.semantic.external_provider_allowed = true;
        config.runtime.enabled = true;

        let base = bench_self_base_config(config);
        assert!(base.security.deny_network);
        assert!(!base.scip.enabled);
        assert!(!base.scip.auto_generate);
        assert!(!base.scip.allow_install);
        assert!(!base.semantic.enabled);
        assert!(!base.semantic.external_provider_allowed);
        assert!(!base.runtime.enabled);
    }

    #[test]
    fn metrics_follow_the_commit_derived_scorer() {
        let score = |rank: Option<usize>, gold_recall: f64| BenchSelfScore { rank, gold_recall };
        let metrics =
            bench_self_metrics(&[score(Some(1), 0.5), score(Some(7), 1.0), score(None, 0.0)])
                .unwrap();
        assert!((metrics.recall_at_5 - 1.0 / 3.0).abs() < 1e-12);
        assert!((metrics.recall_at_20 - 2.0 / 3.0).abs() < 1e-12);
        assert!((metrics.mrr - (1.0 + 1.0 / 7.0) / 3.0).abs() < 1e-12);
        // The metric a best-rank score cannot see: a commit whose other modified files never
        // came back scores the same R@k as one whose whole gold set did.
        assert!((metrics.gold_recall - 0.5).abs() < 1e-12);
        assert_eq!(metrics.cases, 3);
        assert!(bench_self_metrics(&[]).is_none());
    }

    #[test]
    fn rank_order_lists_primary_then_supporting_files_once_relative_to_the_checkout() {
        let worktree = Path::new("/tmp/ok-bench-self-1/base-0");
        let ranked = bench_self_rank_order(
            [
                Path::new("src/a.rs"),
                Path::new("/tmp/ok-bench-self-1/base-0/src/b.rs"),
                Path::new("src/a.rs"),
                Path::new("src/c.rs"),
            ],
            worktree,
        );
        assert_eq!(
            ranked,
            [
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.rs"),
                PathBuf::from("src/c.rs")
            ]
        );
    }

    #[test]
    fn case_reports_rank_every_gold_file_and_redact_paths_and_errors_by_default() {
        let commit = BenchSelfCommit {
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            parent: "fedcba9876543210fedcba9876543210fedcba98".into(),
            query: "cache resolved imports between runs".into(),
            gold: vec![PathBuf::from("src/a.rs"), PathBuf::from("lib/b.rs")],
        };
        let scored = bench_self_case_report(
            0,
            &commit,
            Ok(BenchSelfBaseOutcome::Scored {
                coverage: None,
                ranked: vec![
                    PathBuf::from("lib/b.rs"),
                    PathBuf::from("x.rs"),
                    PathBuf::from("src/a.rs"),
                ],
                scope_boosted: BTreeSet::new(),
                family: open_kioku_core::TaskFamily::IssueToCode,
            }),
            false,
        );
        assert_eq!(scored.status, BenchSelfCaseStatus::Scored);
        assert_eq!(scored.rank, Some(1));
        assert_eq!(scored.commit, None);
        assert_eq!(scored.query, None);
        assert_eq!(scored.task_family, Some("issue_to_code"));
        assert_eq!(scored.returned_files, 3);
        assert_eq!(scored.gold_recall, Some(1.0));
        assert!(!scored.commit_scope_boost_on_gold);
        // The join key under redaction: a digest of the commit, never the commit itself.
        assert!(scored.case_id.starts_with("sha256:"));
        assert!(!scored.case_id.contains(&commit.sha));
        assert_eq!(
            scored.gold,
            [
                BenchSelfGoldRank {
                    path: "src/**/*.rs".into(),
                    rank: Some(3)
                },
                BenchSelfGoldRank {
                    path: "lib/**/*.rs".into(),
                    rank: Some(1)
                },
            ]
        );

        let failed = bench_self_case_report(
            1,
            &commit,
            Err((
                "index",
                anyhow::anyhow!("could not read /private/repo/src/a.rs"),
            )),
            false,
        );
        assert_eq!(failed.status, BenchSelfCaseStatus::Error);
        let message = failed.error.unwrap();
        assert!(message.starts_with("index failed"), "{message}");
        assert!(!message.contains("src/a.rs"), "{message}");

        let revealed = bench_self_case_report(
            1,
            &commit,
            Err(("index", anyhow::anyhow!("could not read src/a.rs"))),
            true,
        );
        assert_eq!(revealed.commit.as_deref(), Some(commit.sha.as_str()));
        assert_eq!(revealed.case_id, commit.sha);
        assert_eq!(revealed.gold[0].path, "src/a.rs");
        assert!(revealed.error.unwrap().contains("src/a.rs"));
    }

    #[test]
    fn families_are_reported_in_declaration_order_and_marked_insufficient_below_the_minimum() {
        let case = |family: &'static str, rank: Option<usize>, status: BenchSelfCaseStatus| {
            BenchSelfCaseReport {
                position: 0,
                case_id: "sha256:0000000000000000".into(),
                commit: None,
                query: None,
                status,
                task_family: Some(family),
                rank,
                gold_recall: rank.map(|_| 1.0),
                gold: Vec::new(),
                returned_files: 0,
                commit_scope_boost_on_gold: false,
                coverage: None,
                error: None,
            }
        };
        let cases = [
            case("general", Some(2), BenchSelfCaseStatus::Scored),
            case("issue_to_code", None, BenchSelfCaseStatus::Scored),
            case("general", Some(30), BenchSelfCaseStatus::Scored),
            case("code_to_test", Some(1), BenchSelfCaseStatus::Error),
        ];
        let section = bench_self_family_section(&cases);
        assert_eq!(
            section
                .families
                .iter()
                .map(|family| (family.family, family.cases))
                .collect::<Vec<_>>(),
            [("issue_to_code", 1), ("general", 2)]
        );
        assert!(section.families.iter().all(|family| family.insufficient));
        assert!((section.families[1].metrics.recall_at_5 - 0.5).abs() < 1e-12);
    }

    #[test]
    fn a_released_checkout_is_unregistered_and_its_directory_and_index_removed() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().canonicalize().unwrap();
        git_in(&repo, &["init", "--quiet"]);
        fs::write(repo.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
        git_in(&repo, &["add", "lib.rs"]);
        git_in(&repo, &["commit", "--quiet", "-m", "add the answer"]);

        let temp = tempfile::tempdir().unwrap();
        let temp_root = temp.path().canonicalize().unwrap();
        let hooks = temp_root.join("hooks");
        fs::create_dir(&hooks).unwrap();
        let worktree = temp_root.join("base-0");
        let cleanup = Mutex::new(BenchSelfCleanup::default());

        bench_self_checkout(&cleanup, &repo, &hooks, &worktree, "HEAD").unwrap();
        fs::create_dir_all(worktree.join(".ok")).unwrap();
        fs::write(worktree.join(".ok").join("index.sqlite"), b"base index").unwrap();
        assert!(bench_self_worktree_registered(&repo, &worktree).unwrap());

        bench_self_lock(&cleanup).release_worktree().unwrap();
        assert!(!worktree.exists());
        assert!(!bench_self_worktree_registered(&repo, &worktree).unwrap());
    }

    #[test]
    fn no_checkout_starts_after_an_interrupt() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().canonicalize().unwrap();
        git_in(&repo, &["init", "--quiet"]);
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().canonicalize().unwrap().join("base-0");
        let cleanup = Mutex::new(BenchSelfCleanup::default());

        bench_self_lock(&cleanup).interrupt().unwrap();
        assert!(bench_self_checkout(&cleanup, &repo, temp.path(), &worktree, "HEAD").is_err());
        assert!(!worktree.exists());
        assert!(bench_self_lock(&cleanup).active_worktree.is_none());
    }

    #[test]
    fn removing_a_checkout_leaves_an_unrelated_stale_worktree_registration_in_place() {
        let repo_dir = tempfile::tempdir().unwrap();
        let repo = repo_dir.path().canonicalize().unwrap();
        git_in(&repo, &["init", "--quiet"]);
        fs::write(repo.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
        git_in(&repo, &["add", "lib.rs"]);
        git_in(&repo, &["commit", "--quiet", "-m", "add the answer"]);

        let temp = tempfile::tempdir().unwrap();
        let temp_root = temp.path().canonicalize().unwrap();
        let hooks = temp_root.join("hooks");
        fs::create_dir(&hooks).unwrap();

        // A registration the user left behind: its directory is gone, git still lists it.
        let unrelated = temp_root.join("users-old-checkout");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                unrelated.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::remove_dir_all(&unrelated).unwrap();
        assert!(bench_self_worktree_registered(&repo, &unrelated).unwrap());

        let worktree = temp_root.join("base-0");
        let cleanup = Mutex::new(BenchSelfCleanup::default());

        // Removed through `git worktree remove`.
        bench_self_checkout(&cleanup, &repo, &hooks, &worktree, "HEAD").unwrap();
        bench_self_lock(&cleanup).release_worktree().unwrap();
        assert!(!bench_self_worktree_registered(&repo, &worktree).unwrap());
        assert!(bench_self_worktree_registered(&repo, &unrelated).unwrap());

        // Its directory gone first, so removed through its own administrative entry.
        bench_self_checkout(&cleanup, &repo, &hooks, &worktree, "HEAD").unwrap();
        fs::remove_dir_all(&worktree).unwrap();
        bench_self_lock(&cleanup).release_worktree().unwrap();
        assert!(!bench_self_worktree_registered(&repo, &worktree).unwrap());
        assert!(bench_self_worktree_registered(&repo, &unrelated).unwrap());
    }

    #[test]
    fn lexical_normalization_resolves_dot_segments_without_the_file_system() {
        assert_eq!(
            bench_self_normalize_lexically(Path::new(
                "/repo/.git/worktrees/base-0/../../../../tmp/./base-0/.git"
            )),
            PathBuf::from("/tmp/base-0/.git")
        );
    }

    /// The failure this gate exists for: an indexing change stops indexing a file class, most
    /// cases become unanswerable, and the few that remain rank well. Without the ratio guard the
    /// headline rises while retrieval got less complete.
    #[test]
    fn metrics_are_suppressed_below_the_floor_or_below_half_the_selected_commits() {
        assert_eq!(bench_self_suppression(12, 20, 10), None);
        assert_eq!(
            bench_self_suppression(2, 20, 10),
            Some("2 cases below the --min-cases floor of 10; 2 of 20 selected commits scored, fewer than half".to_owned())
        );
        // Large run, fixed floor cleared, nine tenths of the corpus gone.
        assert_eq!(
            bench_self_suppression(12, 100, 10),
            Some("12 of 100 selected commits scored, fewer than half".to_owned())
        );
        // Small run, ratio fine, too few cases to support a number.
        assert_eq!(
            bench_self_suppression(4, 5, 10),
            Some("4 cases below the --min-cases floor of 10".to_owned())
        );
    }

    #[test]
    fn coverage_adjusted_scores_count_an_unanswerable_case_as_a_miss_and_skip_errors() {
        let case = |status: BenchSelfCaseStatus, rank: Option<usize>| BenchSelfCaseReport {
            position: 0,
            case_id: "sha256:0000000000000000".into(),
            commit: None,
            query: None,
            status,
            task_family: Some("general"),
            rank,
            gold_recall: rank.map(|_| 1.0),
            gold: Vec::new(),
            returned_files: 0,
            commit_scope_boost_on_gold: false,
            coverage: None,
            error: None,
        };
        let cases = [
            case(BenchSelfCaseStatus::Scored, Some(1)),
            case(BenchSelfCaseStatus::GoldNotIndexed, None),
            case(BenchSelfCaseStatus::Error, None),
        ];
        let scored = bench_self_scored_scores(&cases);
        let adjusted = bench_self_coverage_adjusted_scores(&cases);
        assert_eq!(scored.len(), 1);
        assert_eq!(adjusted.len(), 2);
        assert_eq!(bench_self_metrics(&scored).unwrap().recall_at_5, 1.0);
        // The same run, scored over the cases the index should have been able to answer.
        assert_eq!(bench_self_metrics(&adjusted).unwrap().recall_at_5, 0.5);
    }

    #[test]
    fn digests_are_domain_separated_and_a_case_id_is_short_and_stable() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_ne!(
            bench_self_digest(BENCH_SELF_CASE_DOMAIN, &[sha.to_owned()]),
            bench_self_digest(BENCH_SELF_HEAD_DOMAIN, &[sha.to_owned()])
        );
        let id = bench_self_case_id(sha, false);
        assert_eq!(id, bench_self_case_id(sha, false));
        assert_eq!(id.len(), "sha256:".len() + 16);
        assert_eq!(bench_self_case_id(sha, true), sha);
    }

    #[test]
    fn caveats_state_the_sample_resolution_and_the_join_key_in_the_artifact() {
        let gate = BenchSelfGate {
            min_cases: 10,
            selected_commits: 20,
            scored_cases: 20,
            coverage_adjusted_cases: 20,
            metrics_suppressed: None,
            coverage_adjusted_suppressed: None,
        };
        let metrics = bench_self_metrics(
            &[BenchSelfScore {
                rank: Some(1),
                gold_recall: 1.0,
            }; 20],
        )
        .unwrap();
        let caveats = bench_self_caveats(Some(&metrics), &gate, 0, 0, true, false);
        assert!(
            caveats
                .iter()
                .any(|caveat| caveat.contains("moves R@k by 0.050")),
            "{caveats:?}"
        );
        assert!(caveats.iter().any(|caveat| caveat.contains("case_id")));
        assert!(caveats
            .iter()
            .any(|caveat| caveat.contains("subject_twin_votes")));
        assert!(caveats
            .iter()
            .any(|caveat| caveat.contains("metrics_coverage_adjusted")));

        let suppressed = BenchSelfGate {
            metrics_suppressed: Some("2 cases below the --min-cases floor of 10".to_owned()),
            ..gate
        };
        let caveats = bench_self_caveats(None, &suppressed, 1, 2, false, false);
        assert!(caveats
            .iter()
            .any(|caveat| caveat.contains("no metrics were published")));
        assert!(caveats
            .iter()
            .any(|caveat| caveat.contains("1 cases failed to run")));
        assert!(caveats
            .iter()
            .any(|caveat| caveat.contains("commit-scope path boost fire")));
    }

    #[test]
    fn walking_the_working_head_is_recorded_as_a_caveat() {
        let gate = BenchSelfGate {
            min_cases: 10,
            selected_commits: 20,
            scored_cases: 20,
            coverage_adjusted_cases: 20,
            metrics_suppressed: None,
            coverage_adjusted_suppressed: None,
        };
        let walked = bench_self_caveats(None, &gate, 0, 0, true, true);
        assert!(
            walked
                .iter()
                .any(|caveat| caveat.contains("commits on the working branch are cases")),
            "{walked:?}"
        );
        let pinned = bench_self_caveats(None, &gate, 0, 0, true, false);
        assert!(!pinned
            .iter()
            .any(|caveat| caveat.contains("commits on the working branch are cases")));
    }

    #[test]
    fn printed_precision_never_exceeds_what_the_sample_resolves() {
        assert_eq!(bench_self_decimals(20), 2);
        assert_eq!(bench_self_decimals(99), 2);
        assert_eq!(bench_self_decimals(100), 3);
        assert_eq!(bench_self_decimals(1000), 4);
    }
}

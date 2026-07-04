#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdrRecord {
    id: String,
    title: String,
    status: String,
    decision: String,
    links: AdrLinks,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AdrLinks {
    #[serde(default)]
    components: Vec<String>,
    #[serde(default)]
    boundaries: Vec<String>,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    routes: Vec<String>,
    #[serde(default)]
    contracts: Vec<String>,
    #[serde(default)]
    validation_rules: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AdrExplainOutput {
    task: String,
    matched_adrs: Vec<AdrRecord>,
    caveats: Vec<String>,
    reproduce: Vec<String>,
}

fn handle_adr_command(json: bool, repo: &Path, command: AdrCommand) -> anyhow::Result<()> {
    match command {
        AdrCommand::List { format } => {
            let adrs = load_adrs(repo)?;
            print_adr_records(json, format, &adrs)?;
        }
        AdrCommand::Add {
            title,
            status,
            decision,
            components,
            boundaries,
            files,
            routes,
            contracts,
            validation_rules,
            format,
        } => {
            let mut adrs = load_adrs(repo)?;
            let id = next_adr_id(&adrs, &title);
            let now = chrono::Utc::now().to_rfc3339();
            let adr = AdrRecord {
                id,
                title,
                status,
                decision: decision.unwrap_or_else(|| "Decision recorded locally.".into()),
                links: AdrLinks::from_inputs(
                    components,
                    boundaries,
                    files,
                    routes,
                    contracts,
                    validation_rules,
                ),
                created_at: now.clone(),
                updated_at: now,
            };
            save_adr(repo, &adr)?;
            adrs.push(adr.clone());
            print_adr_records(json, format, &[adr])?;
        }
        AdrCommand::Link {
            id,
            components,
            boundaries,
            files,
            routes,
            contracts,
            validation_rules,
            format,
        } => {
            let mut adrs = load_adrs(repo)?;
            let id = resolve_adr_id(id, &adrs)?;
            let adr = adrs
                .iter_mut()
                .find(|adr| adr.id == id)
                .ok_or_else(|| anyhow::anyhow!("ADR `{id}` was not found"))?;
            adr.links.extend(AdrLinks::from_inputs(
                components,
                boundaries,
                files,
                routes,
                contracts,
                validation_rules,
            ));
            adr.updated_at = chrono::Utc::now().to_rfc3339();
            save_adr(repo, adr)?;
            print_adr_records(json, format, std::slice::from_ref(adr))?;
        }
        AdrCommand::Explain { task, format } => {
            let adrs = load_adrs(repo)?;
            let matched_adrs = governing_adrs_for_task(&task, &adrs);
            let caveats = if matched_adrs.is_empty() {
                vec!["no ADR matched the task text or linked governance facts".into()]
            } else {
                Vec::new()
            };
            let output = AdrExplainOutput {
                task,
                matched_adrs,
                caveats,
                reproduce: vec!["ok adr explain --task <task>".into(), "ok adr list".into()],
            };
            print_adr_explain(json, format, &output)?;
        }
    }
    Ok(())
}

impl AdrLinks {
    fn from_inputs(
        components: Vec<String>,
        boundaries: Vec<String>,
        files: Vec<PathBuf>,
        routes: Vec<String>,
        contracts: Vec<String>,
        validation_rules: Vec<String>,
    ) -> Self {
        let mut links = Self {
            components,
            boundaries,
            files: files
                .into_iter()
                .map(|path| normalize_path_string(&path))
                .collect(),
            routes,
            contracts,
            validation_rules,
        };
        links.normalize();
        links
    }

    fn extend(&mut self, other: AdrLinks) {
        self.components.extend(other.components);
        self.boundaries.extend(other.boundaries);
        self.files.extend(other.files);
        self.routes.extend(other.routes);
        self.contracts.extend(other.contracts);
        self.validation_rules.extend(other.validation_rules);
        self.normalize();
    }

    fn normalize(&mut self) {
        normalize_strings(&mut self.components);
        normalize_strings(&mut self.boundaries);
        normalize_strings(&mut self.files);
        normalize_strings(&mut self.routes);
        normalize_strings(&mut self.contracts);
        normalize_strings(&mut self.validation_rules);
    }

    fn is_empty(&self) -> bool {
        self.components.is_empty()
            && self.boundaries.is_empty()
            && self.files.is_empty()
            && self.routes.is_empty()
            && self.contracts.is_empty()
            && self.validation_rules.is_empty()
    }
}

fn load_adrs(repo: &Path) -> anyhow::Result<Vec<AdrRecord>> {
    let dir = adr_dir(repo);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let mut record: AdrRecord =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        record.links.normalize();
        records.push(record);
    }
    records.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(records)
}

fn save_adr(repo: &Path, adr: &AdrRecord) -> anyhow::Result<()> {
    let dir = adr_dir(repo);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.toml", adr.id));
    fs::write(path, toml::to_string_pretty(adr)?)?;
    Ok(())
}

fn adr_dir(repo: &Path) -> PathBuf {
    repo.join(".open-kioku/adrs")
}

fn next_adr_id(existing: &[AdrRecord], title: &str) -> String {
    let next = existing
        .iter()
        .filter_map(|adr| adr.id.strip_prefix("adr-"))
        .filter_map(|suffix| suffix.split('-').next())
        .filter_map(|number| number.parse::<usize>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    format!("adr-{next:04}-{}", slugify(title))
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "decision".into()
    } else {
        slug.chars().take(48).collect()
    }
}

fn resolve_adr_id(id: Option<String>, adrs: &[AdrRecord]) -> anyhow::Result<String> {
    if let Some(id) = id {
        return Ok(id);
    }
    match adrs {
        [] => anyhow::bail!("no ADRs exist; run `ok adr add <title>` first"),
        [adr] => Ok(adr.id.clone()),
        _ => anyhow::bail!("multiple ADRs exist; pass the ADR id to `ok adr link`"),
    }
}

fn governing_adrs_for_plan(report: &PlanReport, adrs: &[AdrRecord]) -> Vec<AdrRecord> {
    let mut facts = vec![report.task.clone()];
    facts.extend(
        report
            .primary_context
            .iter()
            .map(|result| result.path.display().to_string()),
    );
    facts.extend(
        report
            .recommended_change_boundary
            .allowed_files
            .iter()
            .map(|file| file.display().to_string()),
    );
    facts.extend(
        report
            .recommended_change_boundary
            .caution_files
            .iter()
            .map(|file| file.display().to_string()),
    );
    facts.extend(
        report
            .relevant_symbols
            .iter()
            .map(|symbol| symbol.qualified_name.clone()),
    );
    facts.extend(
        report
            .recommended_change_boundary
            .signal_hooks
            .architecture_components
            .iter()
            .cloned(),
    );
    matching_adrs(&facts, adrs)
}

fn governing_adrs_for_changed_files(
    changed_files: &[PathBuf],
    adrs: &[AdrRecord],
) -> Vec<AdrRecord> {
    let facts = changed_files
        .iter()
        .map(normalize_path_string)
        .collect::<Vec<_>>();
    matching_adrs(&facts, adrs)
}

fn governing_adrs_for_task(task: &str, adrs: &[AdrRecord]) -> Vec<AdrRecord> {
    matching_adrs(&[task.to_string()], adrs)
}

fn matching_adrs(facts: &[String], adrs: &[AdrRecord]) -> Vec<AdrRecord> {
    let facts = facts
        .iter()
        .map(|fact| normalize_match_text(fact))
        .collect::<Vec<_>>();
    adrs.iter()
        .filter(|adr| adr_matches_facts(adr, &facts))
        .cloned()
        .collect()
}

fn adr_matches_facts(adr: &AdrRecord, facts: &[String]) -> bool {
    let mut needles = vec![adr.id.clone(), adr.title.clone()];
    needles.extend(adr.links.components.clone());
    needles.extend(adr.links.boundaries.clone());
    needles.extend(adr.links.files.clone());
    needles.extend(adr.links.routes.clone());
    needles.extend(adr.links.contracts.clone());
    needles.extend(adr.links.validation_rules.clone());
    needles
        .into_iter()
        .map(|needle| normalize_match_text(&needle))
        .filter(|needle| !needle.is_empty())
        .any(|needle| {
            facts
                .iter()
                .any(|fact| fact == &needle || fact.contains(&needle) || needle.contains(fact))
        })
}

fn annotate_contract_with_adrs(
    contract: &mut ChangeContractV1,
    plan: &PlanReport,
    repo: &Path,
) -> anyhow::Result<Vec<AdrRecord>> {
    let adrs = governing_adrs_for_plan(plan, &load_adrs(repo)?);
    if adrs.is_empty() {
        return Ok(adrs);
    }
    let adr_refs = adrs
        .iter()
        .map(|adr| format!("adr:{}", adr.id))
        .collect::<Vec<_>>();
    for evidence_ref in &adr_refs {
        if !contract
            .evidence_refs
            .iter()
            .any(|existing| existing.0 == *evidence_ref)
        {
            contract.evidence_refs.push(EvidenceRef::new(evidence_ref));
        }
    }
    for adr in &adrs {
        let evidence_ref = EvidenceRef::new(format!("adr:{}", adr.id));
        if !contract
            .architecture_constraints
            .iter()
            .any(|constraint| constraint.rule == format!("adr-governance:{}", adr.id))
        {
            contract
                .architecture_constraints
                .push(ArchitectureConstraint {
                    rule: format!("adr-governance:{}", adr.id),
                    severity: ConstraintSeverity::Advisory,
                    reason: format!("ADR `{}` governs this touched area: {}", adr.id, adr.title),
                    evidence_refs: vec![evidence_ref],
                });
        }
    }
    contract.extensions.insert(
        "adr_governance".into(),
        serde_json::json!(
            adrs.iter()
                .map(|adr| serde_json::json!({
                    "id": adr.id,
                    "title": adr.title,
                    "status": adr.status,
                    "links": adr.links,
                }))
                .collect::<Vec<_>>()
        ),
    );
    Ok(adrs)
}

fn print_adr_records(json: bool, format: AdrFormat, adrs: &[AdrRecord]) -> anyhow::Result<()> {
    match effective_adr_format(json, format) {
        AdrFormat::Json => println!("{}", serde_json::to_string_pretty(adrs)?),
        AdrFormat::Markdown => print!("{}", render_adr_markdown(adrs)),
        AdrFormat::Text => {
            if adrs.is_empty() {
                println!("No ADRs found.");
            } else {
                for adr in adrs {
                    println!("{} [{}] {}", adr.id, adr.status, adr.title);
                    if !adr.links.is_empty() {
                        println!("  links: {}", adr_links_summary(&adr.links));
                    }
                }
            }
        }
    }
    Ok(())
}

fn print_adr_explain(
    json: bool,
    format: AdrFormat,
    output: &AdrExplainOutput,
) -> anyhow::Result<()> {
    match effective_adr_format(json, format) {
        AdrFormat::Json => println!("{}", serde_json::to_string_pretty(output)?),
        AdrFormat::Markdown => {
            println!("# ADR Explanation\n");
            println!("Task: `{}`\n", output.task);
            if output.matched_adrs.is_empty() {
                println!("- No ADRs matched.");
            } else {
                for adr in &output.matched_adrs {
                    println!("- `{}`: {} ({})", adr.id, adr.title, adr.status);
                }
            }
            for caveat in &output.caveats {
                println!("- Caveat: {caveat}");
            }
        }
        AdrFormat::Text => {
            println!("ADR governance for task: {}", output.task);
            if output.matched_adrs.is_empty() {
                println!("  - none matched");
            } else {
                for adr in &output.matched_adrs {
                    println!("  - {} [{}] {}", adr.id, adr.status, adr.title);
                }
            }
            for caveat in &output.caveats {
                println!("caveat: {caveat}");
            }
        }
    }
    Ok(())
}

fn effective_adr_format(json: bool, format: AdrFormat) -> AdrFormat {
    if json {
        AdrFormat::Json
    } else {
        format
    }
}

fn render_adr_markdown(adrs: &[AdrRecord]) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Decision Records\n\n");
    if adrs.is_empty() {
        out.push_str("No ADRs found.\n");
        return out;
    }
    for adr in adrs {
        out.push_str(&format!("## `{}` {}\n\n", adr.id, adr.title));
        out.push_str(&format!("- Status: `{}`\n", adr.status));
        out.push_str(&format!("- Decision: {}\n", adr.decision));
        out.push_str(&format!("- Links: {}\n\n", adr_links_summary(&adr.links)));
    }
    out
}

fn adr_links_summary(links: &AdrLinks) -> String {
    let mut parts = Vec::new();
    push_link_part(&mut parts, "components", &links.components);
    push_link_part(&mut parts, "boundaries", &links.boundaries);
    push_link_part(&mut parts, "files", &links.files);
    push_link_part(&mut parts, "routes", &links.routes);
    push_link_part(&mut parts, "contracts", &links.contracts);
    push_link_part(&mut parts, "validation", &links.validation_rules);
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join("; ")
    }
}

fn push_link_part(parts: &mut Vec<String>, label: &str, values: &[String]) {
    if !values.is_empty() {
        parts.push(format!("{label}: {}", values.join(", ")));
    }
}

fn normalize_strings(values: &mut Vec<String>) {
    let mut set = BTreeSet::new();
    for value in values.drain(..) {
        let value = value.trim().replace('\\', "/");
        if !value.is_empty() {
            set.insert(value);
        }
    }
    values.extend(set);
}

fn normalize_path_string(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().replace('\\', "/")
}

fn normalize_match_text(value: &str) -> String {
    value.trim().replace('\\', "/").to_ascii_lowercase()
}

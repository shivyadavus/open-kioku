use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

fn ok() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ok"))
}

fn run(mut command: Command) -> String {
    let output = command.output().expect("command should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

fn run_ok_with_stderr(mut command: Command) -> (String, String) {
    let output = command.output().expect("command should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf-8"),
        String::from_utf8(output.stderr).expect("stderr should be utf-8"),
    )
}

fn run_failure(mut command: Command) -> (String, String) {
    let output = command.output().expect("command should run");
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf-8"),
        String::from_utf8(output.stderr).expect("stderr should be utf-8"),
    )
}

fn run_with_stdin(mut command: Command, stdin: &str) -> String {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("command should spawn");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("stdin should write");
    let output = child.wait_with_output().expect("command should finish");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

#[test]
fn version_json_reports_machine_readable_metadata() {
    for args in [
        ["--version", "--json"],
        ["--json", "--version"],
        ["-V", "--json"],
        ["--json", "-V"],
    ] {
        let output = run({
            let mut command = ok();
            command.args(args);
            command
        });
        assert_eq!(
            output.lines().count(),
            1,
            "version report should be a single JSON line, got: {output}"
        );
        let report: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(report["name"], "ok");
        assert_eq!(report["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(report["crate"], "open-kioku-cli");
    }

    let plain = run({
        let mut command = ok();
        command.arg("--version");
        command
    });
    assert_eq!(plain.trim(), format!("ok {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn subcommand_help_includes_copyable_examples() {
    for (subcommand, example) in [
        ("search", "ok search \"token refresh\""),
        ("impact", "ok impact --file src/auth.rs"),
        ("status", "ok status --markdown --write ok-status.md"),
    ] {
        let help = run({
            let mut command = ok();
            command.arg(subcommand).arg("--help");
            command
        });
        assert!(
            help.contains("Examples:"),
            "{subcommand} --help should list examples:\n{help}"
        );
        assert!(
            help.contains(example),
            "{subcommand} --help should include `{example}`:\n{help}"
        );
    }
}

/// RI3.6 phase 1: a legacy `.ok` layout adopts the generation layout on the next
/// `ok index` (a directory move under the write lock), the active pointer publishes
/// atomically, and every read path resolves through it transparently.
#[test]
fn indexing_adopts_legacy_layout_into_generations_and_reads_still_work() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn generation_probe() -> u8 { 7 }\n",
    )
    .unwrap();

    // Build a legacy-layout index first (init + index on a fresh repo produces the
    // generation layout directly, so simulate legacy by moving components back out).
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let location = open_kioku_storage::generations::resolve_index_location(repo);
    if let Some(generation) = location.generation_id() {
        // Un-adopt: move components back to the legacy layout and drop the pointer.
        let generation_dir = repo.join(".ok/generations").join(generation);
        for name in ["index.sqlite", "search", "vectors"] {
            let source = generation_dir.join(name);
            if source.exists() {
                fs::rename(&source, repo.join(".ok").join(name)).unwrap();
            }
        }
        fs::remove_dir_all(repo.join(".ok/generations")).unwrap();
    }
    assert!(repo.join(".ok/index.sqlite").exists());
    assert_eq!(
        open_kioku_storage::generations::resolve_index_location(repo).generation_id(),
        None,
        "fixture should now be a legacy layout"
    );

    // Reads work on the legacy layout.
    let legacy_lookup = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("symbol")
            .arg("definition")
            .arg("generation_probe");
        command
    });
    assert!(legacy_lookup.contains("generation_probe"));

    // The next index adopts the layout.
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    let adopted = open_kioku_storage::generations::resolve_index_location(repo);
    assert!(
        adopted.generation_id().is_some(),
        "index should have adopted the generation layout"
    );
    assert!(repo.join(".ok/generations/active").exists());
    assert!(!repo.join(".ok/index.sqlite").exists());

    // Reads resolve through the generation transparently.
    let generation_lookup = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("symbol")
            .arg("definition")
            .arg("generation_probe");
        command
    });
    assert!(generation_lookup.contains("generation_probe"));

    // Status reports the generation identity.
    let status = run({
        let mut command = ok();
        command
            .arg("status")
            .arg(repo)
            .arg("--markdown")
            .arg("--write")
            .arg(repo.join("status.md"));
        command
    });
    drop(status);
    let status_markdown = fs::read_to_string(repo.join("status.md")).unwrap();
    assert!(status_markdown.contains("Index generation"));
}

#[test]
fn agent_setup_is_safe_idempotent_and_verifies_local_mcp() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join(".cursor")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"onboarding-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n").unwrap();
    fs::write(
        repo.join(".cursor/mcp.json"),
        r#"{"mcpServers":{"unrelated":{"command":"other"}}}"#,
    )
    .unwrap();

    let dry_run = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("cursor")
            .arg("--repo")
            .arg(repo);
        command
    });
    assert!(dry_run.contains("dry_run"));
    assert!(!repo.join(".ok").exists());

    let applied = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("cursor")
            .arg("--repo")
            .arg(repo)
            .arg("--apply");
        command
    });
    assert!(applied.contains("[passed] mcp_stdio"));
    assert!(repo.join(".ok/index.sqlite").is_file());
    assert!(repo
        .join(".cursor/rules/open-kioku-preflight.mdc")
        .is_file());
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["unrelated"]["command"], "other");
    assert_eq!(config["mcpServers"]["open-kioku"]["command"], "ok");

    let checked = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("cursor")
            .arg("--repo")
            .arg(repo)
            .arg("--check");
        command
    });
    assert!(checked.contains("Open Kioku is ready for this repository."));

    let uninstalled = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("cursor")
            .arg("--repo")
            .arg(repo)
            .arg("--uninstall");
        command
    });
    assert!(uninstalled.contains("[removed] config"));
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(config["mcpServers"]["unrelated"]["command"], "other");
    assert!(config["mcpServers"].get("open-kioku").is_none());
    assert!(repo.join(".ok/index.sqlite").is_file());
}

#[test]
fn preflight_has_cli_mcp_parity() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"preflight-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn issue_token(subject: &str) -> String { subject.to_owned() }\n",
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let cli = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("preflight")
            .arg("add token expiration");
        command
    });
    let cli: serde_json::Value = serde_json::from_str(&cli).unwrap();
    assert!(cli["verdict"].is_string());
    assert!(cli["confidence"].is_string());
    assert!(cli["evidence_quality"].is_object());
    assert!(cli["edit_files"].is_array());
    assert!(cli["evidence_refs"].is_array());

    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"plan_change","arguments":{"task":"add token expiration","detail":"preflight"}}}"#,
    );
    let mcp: serde_json::Value = serde_json::from_str(mcp.trim()).unwrap();
    let mcp = &mcp["result"]["structuredContent"];
    assert_eq!(mcp["verdict"], cli["verdict"]);
    assert_eq!(mcp["confidence"], cli["confidence"]);
    assert_eq!(mcp["task"], cli["task"]);
}

/// `ok search` and MCP `search_code` answer through one ranking (#448). The repository's
/// `[ranking]` weights reorder the lexical order here, so a surface that skipped the rerank, or
/// read other weights, returns a different list.
#[test]
fn search_has_cli_mcp_parity_in_order_and_pages() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("tests")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"search-parity-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // Source files mention the term more often than the test files, so lexical relevance lists
    // them first; a high `validation_proximity` weight lists the test files first once ranked.
    for (path, name, mentions) in [
        ("src/alpha.rs", "alpha", 9),
        ("src/gamma.rs", "gamma", 6),
        ("src/delta.rs", "delta", 3),
        ("src/zeta.rs", "zeta", 1),
        ("tests/beta.rs", "beta", 1),
        ("tests/epsilon.rs", "epsilon", 2),
    ] {
        fs::write(
            repo.join(path),
            format!(
                "pub fn run_{name}() -> &'static str {{\n    \"{}\"\n}}\n",
                "reconcile ".repeat(mentions).trim_end()
            ),
        )
        .unwrap();
    }
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    let mut rewritten = 0;
    let config = fs::read_to_string(repo.join("ok.toml"))
        .unwrap()
        .lines()
        .map(|line| {
            if line.starts_with("text_relevance =") {
                rewritten += 1;
                "text_relevance = 0.01".to_string()
            } else if line.starts_with("validation_proximity =") {
                rewritten += 1;
                "validation_proximity = 100.0".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        rewritten, 2,
        "ok.toml should carry both [ranking] weights:\n{config}"
    );
    fs::write(repo.join("ok.toml"), config + "\n").unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    fn ranked_identity(results: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
        results
            .as_array()
            .expect("results should be an array")
            .iter()
            .map(|result| {
                (
                    result["path"].as_str().expect("path").to_string(),
                    result["line_range"].clone(),
                )
            })
            .collect()
    }

    let modes = [("code", None), ("hybrid", Some("--hybrid"))];
    let mut requests = String::new();
    for (mode, _) in modes {
        for (id, limit, offset) in [("all", 6, 0), ("p0", 2, 0), ("p1", 2, 2), ("p2", 2, 4)] {
            requests.push_str(
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": format!("{mode}-{id}"),
                    "method": "tools/call",
                    "params": {
                        "name": "search_code",
                        "arguments": {"query": "reconcile", "mode": mode, "limit": limit, "offset": offset}
                    }
                })
                .to_string(),
            );
            requests.push('\n');
        }
    }
    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &requests,
    );
    let responses = mcp
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let response: serde_json::Value = serde_json::from_str(line).unwrap();
            (
                response["id"].as_str().expect("response id").to_string(),
                response["result"]["structuredContent"].clone(),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();

    for (mode, flag) in modes {
        let cli = run({
            let mut command = ok();
            command.arg("--repo").arg(repo).arg("--json").arg("search");
            if let Some(flag) = flag {
                command.arg(flag);
            }
            command.arg("reconcile").arg("--limit").arg("6");
            command
        });
        let cli: serde_json::Value = serde_json::from_str(&cli).unwrap();
        let cli_order = ranked_identity(&cli["results"]);
        assert_eq!(
            cli_order.len(),
            6,
            "{mode}: every fixture file should match: {cli}"
        );
        // Without this the test could not tell a reranked list from the index's own order.
        let lexical = cli["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|result| {
                result["score_breakdown"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|component| component["signal"] == "text_relevance")
                    .and_then(|component| component["raw_value"].as_f64())
                    .expect("every result should carry text_relevance")
            })
            .collect::<Vec<_>>();
        assert!(
            lexical.windows(2).any(|pair| pair[0] < pair[1]),
            "{mode}: the fixture must rank against lexical order: {lexical:?}"
        );

        let all = &responses[&format!("{mode}-all")];
        assert_eq!(
            ranked_identity(&all["results"]),
            cli_order,
            "{mode}: search_code must return ok search's order: {all}"
        );
        assert_eq!(all["has_more"], false, "{mode}: {all}");

        let mut paged = Vec::new();
        for (page, has_more) in [("p0", true), ("p1", true), ("p2", false)] {
            let response = &responses[&format!("{mode}-{page}")];
            assert_eq!(response["has_more"], has_more, "{mode} {page}: {response}");
            paged.extend(ranked_identity(&response["results"]));
        }
        assert_eq!(
            paged, cli_order,
            "{mode}: pages must be slices of one ranking"
        );
    }
    assert!(responses["hybrid-all"]["semantic_status"].is_object());
}

/// A deep `search_code` page is served, and it is the matching slice of `ok search`'s list: the
/// candidate pool grows with `offset + limit` up to the 500 candidates `search_code` could page
/// through before the two surfaces shared one ranking (#448).
#[test]
fn deep_search_pages_have_cli_mcp_parity() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"deep-search-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    for index in 0..520 {
        fs::write(
            repo.join(format!("src/unit_{index:03}.rs")),
            format!("pub fn unit_{index:03}() -> &'static str {{\n    \"ledgerline\"\n}}\n"),
        )
        .unwrap();
    }
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let cli = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("ledgerline")
            .arg("--limit")
            .arg("170");
        command
    });
    let cli: serde_json::Value = serde_json::from_str(&cli).unwrap();
    let cli = cli["results"]
        .as_array()
        .expect("ok search prints its results")
        .clone();
    assert_eq!(cli.len(), 170, "every fixture file matches the query");

    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        concat!(
            r#"{"jsonrpc":"2.0","id":"deep","method":"tools/call","params":{"name":"search_code","arguments":{"query":"ledgerline","limit":20,"offset":150}}}"#,
            "\n"
        ),
    );
    let response: serde_json::Value = serde_json::from_str(mcp.trim()).unwrap();
    let page = &response["result"]["structuredContent"];
    let results = page["results"].as_array().expect("results");
    assert_eq!(results.len(), 20, "{page}");
    assert_eq!(page["has_more"], true, "{page}");
    assert_eq!(page["truncated"], false, "{page}");

    let identity =
        |result: &serde_json::Value| (result["path"].clone(), result["line_range"].clone());
    assert_eq!(
        results.iter().map(identity).collect::<Vec<_>>(),
        cli[150..170].iter().map(identity).collect::<Vec<_>>(),
        "search_code offset 150 must be ok search's results 150..170"
    );

    // Past the 500-candidate window both surfaces rank, each must say so in the same words:
    // a page that ends the ranking from a filled window is short of the index.
    let cli_capped = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("ledgerline")
            .arg("--limit")
            .arg("520");
        command
    });
    let cli_capped: serde_json::Value = serde_json::from_str(&cli_capped).unwrap();
    assert_eq!(cli_capped["truncated"], true, "{cli_capped}");
    let cli_warning = cli_capped["warnings"][0]
        .as_str()
        .expect("ok search reports the filled window")
        .to_string();

    let mcp_capped = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        concat!(
            r#"{"jsonrpc":"2.0","id":"capped","method":"tools/call","params":{"name":"search_code","arguments":{"query":"ledgerline","limit":100,"offset":450}}}"#,
            "\n"
        ),
    );
    let mcp_capped: serde_json::Value = serde_json::from_str(mcp_capped.trim()).unwrap();
    let mcp_capped = &mcp_capped["result"]["structuredContent"];
    assert_eq!(mcp_capped["truncated"], true, "{mcp_capped}");
    assert_eq!(
        mcp_capped["warnings"][0].as_str(),
        Some(cli_warning.as_str()),
        "both surfaces must report a filled candidate window in the same words: {mcp_capped}"
    );
}

#[test]
fn history_bench_covers_public_api_families() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = repo.join("benchmarks/history-cases.json");
    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("history")
            .arg("bench")
            .arg("--cases-file")
            .arg(cases);
        command
    });
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["reviewer_accuracy"], 1.0);
    assert_eq!(report["similar_recall_at_5"], 1.0);
    assert_eq!(report["family_counts"]["similar"], 2);
    assert_eq!(report["family_counts"]["ownership"], 1);
    assert_eq!(report["family_counts"]["reviewers"], 1);
    assert_eq!(report["family_counts"]["churn"], 3);
    assert_eq!(report["family_counts"]["provenance"], 1);
    assert!(report["failures"].as_array().unwrap().is_empty());
}

#[test]
fn architecture_policy_validate_and_print_are_index_independent() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let policy_dir = repo.join(".open-kioku");
    fs::create_dir_all(&policy_dir).unwrap();
    fs::write(
        policy_dir.join("architecture.toml"),
        include_str!("../../../examples/architecture-policy.toml"),
    )
    .unwrap();

    let validation = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    let validation: serde_json::Value = serde_json::from_str(&validation).unwrap();
    assert_eq!(validation["valid"], true);
    assert_eq!(validation["configured"], true);
    assert_eq!(validation["source"], "canonical");
    assert_eq!(validation["policy"]["version"], "v1");

    let validation_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("validate")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(validation_markdown.contains("# Architecture Policy Validation"));
    assert!(validation_markdown.contains("- Layers:"));

    let printed = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("print");
        command
    });
    assert!(printed.contains("# source: canonical"));
    assert!(printed.contains("version = \"v1\""));
    assert!(printed.contains("api-must-not-depend-on-storage"));

    let explicit = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("validate")
            .arg("--path")
            .arg(".open-kioku/architecture.toml");
        command
    });
    let explicit: serde_json::Value = serde_json::from_str(&explicit).unwrap();
    assert_eq!(explicit["source"], "explicit");

    let no_policy = run({
        let empty = tempfile::tempdir().unwrap();
        let mut command = ok();
        command
            .arg("--repo")
            .arg(empty.path())
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    assert!(no_policy.contains("Heuristic architecture detection remains active"));

    fs::write(
        policy_dir.join("architecture.toml"),
        include_str!("../../../examples/architecture-policy.toml")
            .replace("severity = \"error\"", "severity = \"urgent\""),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    assert!(stderr.contains("architecture.toml"));
    assert!(stderr.contains("unknown variant `urgent`"));
}

#[test]
fn architecture_policy_check_and_explain_public_api_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join(".open-kioku")).unwrap();
    fs::create_dir_all(repo.join("src/api/internal")).unwrap();
    fs::create_dir_all(repo.join("src/domain")).unwrap();
    fs::write(
        repo.join(".open-kioku/architecture.toml"),
        r#"version = "v1"

[[layers]]
id = "api"
paths = ["src/api/**"]

[[layers]]
id = "domain"
paths = ["src/domain/**"]

[[public_api_rules]]
id = "api-public-boundary"
component = "api"
public_globs = ["src/api/mod.rs"]
internal_globs = ["src/api/internal/**"]
severity = "error"
reason = "domain code must use the api facade"
"#,
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub mod api;\npub mod domain;\n").unwrap();
    fs::write(repo.join("src/api/mod.rs"), "pub mod internal;\n").unwrap();
    fs::write(
        repo.join("src/api/internal/mod.rs"),
        "pub struct Session;\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/domain/mod.rs"),
        "use crate::api::internal;\npub fn leak() -> internal::Session { internal::Session }\n",
    )
    .unwrap();

    let _ = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let check = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("check");
        command
    });
    let check: serde_json::Value = serde_json::from_str(&check).unwrap();
    assert_eq!(check["public_api_violation_count"], 1);
    assert_eq!(check["violations"][0]["rule_id"], "api-public-boundary");

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("plan")
            .arg("domain");
        command
    });
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(plan["architecture_policy"]["configured"], true);
    assert_eq!(plan["impact"]["architecture_policy"]["configured"], true);
    assert_eq!(
        plan["architecture_policy"]["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let impact = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("impact")
            .arg("--file")
            .arg("src/domain/mod.rs");
        command
    });
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(impact["architecture_policy"]["configured"], true);
    assert_eq!(
        impact["architecture_policy"]["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let check_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("check")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(check_markdown.contains("# Architecture Policy Check"));
    assert!(check_markdown.contains("api-public-boundary"));

    let explain = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("explain")
            .arg("--file")
            .arg("src/api/internal/mod.rs");
        command
    });
    let explain: serde_json::Value = serde_json::from_str(&explain).unwrap();
    assert_eq!(explain["configured"], true);
    assert_eq!(explain["components"][0]["component_id"], "api");
    assert_eq!(explain["violations"][0]["rule_id"], "api-public-boundary");

    let repo_explain = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("explain");
        command
    });
    let repo_explain: serde_json::Value = serde_json::from_str(&repo_explain).unwrap();
    assert_eq!(repo_explain["query_kind"], "repo");
    assert_eq!(
        repo_explain["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let explain_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("explain")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(explain_markdown.contains("# Architecture Policy Explanation"));
    assert!(explain_markdown.contains("api-public-boundary"));

    // `architecture_policy_validate` and `architecture_policy_explain` left the
    // MCP surface in 4.0.0; the CLI paths asserted above are what ships. The
    // summary is the CLI replacement for the retired `summarize_architecture`,
    // and `violations` now reports the evaluated policy rather than heuristics.
    let summary = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("summary");
        command
    });
    let summary: serde_json::Value = serde_json::from_str(&summary).unwrap();
    assert_eq!(summary["configured"], true);
    assert_eq!(summary["policy_source"], "canonical");
    assert_eq!(summary["policy_check"]["configured"], true);
    assert_eq!(summary["violations"][0]["rule_id"], "api-public-boundary");

    let violations = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("violations");
        command
    });
    let violations: serde_json::Value = serde_json::from_str(&violations).unwrap();
    assert_eq!(violations["configured"], true);
    assert_eq!(
        violations["violations"][0]["rule_id"], "api-public-boundary",
        "violations must come from the evaluated policy, not heuristic detection"
    );

    let mcp_plan = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"plan_change","arguments":{"task":"domain","limit":5,"format":"json"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_plan.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["architecture_policy"]["configured"],
        true
    );

    let mcp_impact = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"impact_analysis","arguments":{"path":"src/domain/mod.rs"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_impact.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["architecture_policy"]["configured"],
        true
    );
}

#[test]
fn trust_reports_adrs_and_html_outputs_are_source_safe() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("tests")).unwrap();
    fs::write(
        repo.join("src/auth.rs"),
        r#"
pub fn issue_token(user: &str) -> String {
    let secret_token_value = "do-not-leak-this-source";
    format!("{user}:{secret_token_value}")
}
"#,
    )
    .unwrap();
    fs::write(
        repo.join("tests/auth_test.rs"),
        r#"
#[test]
fn auth_flow() {
    assert!(true);
}
"#,
    )
    .unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let added = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("adr")
            .arg("add")
            .arg("Auth boundary")
            .arg("--component")
            .arg("auth")
            .arg("--file")
            .arg("src/auth.rs")
            .arg("--validation-rule")
            .arg("cargo test")
            .arg("--decision")
            .arg("Authentication code owns token checks");
        command
    });
    let added: serde_json::Value = serde_json::from_str(&added).unwrap();
    let adr_id = added[0]["id"].as_str().unwrap().to_string();

    let linked = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("adr")
            .arg("link")
            .arg(&adr_id)
            .arg("--boundary")
            .arg("auth-boundary")
            .arg("--contract")
            .arg("token-contract");
        command
    });
    assert!(linked.contains(&adr_id));

    let adr_explain = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("adr")
            .arg("explain")
            .arg("--task")
            .arg("change auth token flow");
        command
    });
    assert!(adr_explain.contains(&adr_id));

    for subcommand in ["overview", "clusters", "hotspots", "boundaries", "drift"] {
        let output = run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .arg("architecture")
                .arg(subcommand);
            command
        });
        assert!(output.contains("Architecture"), "{subcommand}: {output}");
        assert!(
            output.contains("Validation Requirements") || output.contains("Components"),
            "{subcommand}: {output}"
        );
        assert!(!output.contains("do-not-leak-this-source"));
    }
    let hotspots = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("hotspots");
        command
    });
    assert!(hotspots.contains("High-risk Files"));
    assert!(hotspots.contains("src/auth.rs"));

    let ui = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("ui");
        command
    });
    assert!(ui.contains("<!doctype html>"));
    assert!(ui.contains("Task"));
    assert!(ui.contains("Verification result"));
    assert!(!ui.contains("do-not-leak-this-source"));

    let plan_html = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("plan")
            .arg("change auth token flow")
            .arg("--format")
            .arg("html");
        command
    });
    assert!(plan_html.contains("<!doctype html>"));
    assert!(plan_html.contains("ADR Governance"));
    assert!(plan_html.contains(&adr_id));
    assert!(!plan_html.contains("do-not-leak-this-source"));

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("plan")
            .arg("change auth token flow")
            .arg("--format")
            .arg("json");
        command
    });
    let plan_path = repo.join("plan.json");
    fs::write(&plan_path, &plan_json).unwrap();

    let contract = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("contract")
            .arg("create")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--no-store")
            .arg("--format")
            .arg("json");
        command
    });
    let contract: serde_json::Value = serde_json::from_str(&contract).unwrap();
    assert_eq!(
        contract["contract"]["adr_governance"][0]["id"].as_str(),
        Some(adr_id.as_str())
    );

    let verify_html = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--format")
            .arg("html")
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    assert!(verify_html.contains("<!doctype html>"));
    assert!(verify_html.contains("Verification"));
    assert!(verify_html.contains(&adr_id));
    assert!(!verify_html.contains("do-not-leak-this-source"));

    let proof_html = run({
        let mut command = ok();
        command
            .arg("prove")
            .arg(repo)
            .arg("--task")
            .arg("auth")
            .arg("--html");
        command
    });
    assert!(proof_html.contains("<!doctype html>"));
    assert!(proof_html.contains("Open Kioku Proof"));
    assert!(!proof_html.contains("do-not-leak-this-source"));
}

#[test]
fn verify_enforces_configured_architecture_policy_without_dependency_flag() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join(".open-kioku")).unwrap();
    fs::create_dir_all(repo.join("src/domain")).unwrap();
    fs::create_dir_all(repo.join("src/api")).unwrap();
    fs::write(
        repo.join(".open-kioku/architecture.toml"),
        r#"version = "v1"

[[layers]]
id = "domain"
paths = ["src/domain/**"]

[[layers]]
id = "api"
paths = ["src/api/**"]

[[dependency_rules]]
id = "domain-must-not-import-api"
from = "domain"
to = "api"
action = "forbid"
severity = "error"
reason = "domain cannot import api"
"#,
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub mod api;\npub mod domain;\n").unwrap();
    fs::write(repo.join("src/api/mod.rs"), "pub mod secret;\n").unwrap();
    fs::write(repo.join("src/api/secret.rs"), "pub fn secret() {}\n").unwrap();
    fs::write(repo.join("src/domain/mod.rs"), "pub mod order;\n").unwrap();
    fs::write(
        repo.join("src/domain/order.rs"),
        "pub fn order() -> u32 { 1 }\n",
    )
    .unwrap();

    let _ = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("plan")
            .arg("order")
            .arg("--limit")
            .arg("5");
        command
    });
    let plan_path = repo.join("plan.json");
    fs::write(&plan_path, &plan_json).unwrap();

    fs::write(
        repo.join("src/domain/order.rs"),
        "use crate::api::secret;\npub fn order() -> u32 { secret(); 1 }\n",
    )
    .unwrap();

    let (stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/domain/order.rs");
        command
    });
    assert!(stdout.contains("\"verdict\": \"fail\""));
    assert!(stdout.contains("domain-must-not-import-api"));
    assert!(stdout.contains("dependency_deltas"));
    assert!(stderr.contains("change verification failed"));

    let plan_value: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "plan": plan_value,
                "changed_files": ["src/domain/order.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    let response: serde_json::Value = serde_json::from_str(mcp_verify.trim()).unwrap();
    assert_eq!(response["result"]["structuredContent"]["verdict"], "fail");
    assert!(mcp_verify.contains("domain-must-not-import-api"));
}

/// `ok index` stores no git history naming a path discovery never reads (#525): not in the
/// touch, co-change, symbol-touch or hotspot tables, and not in any fact or graph row derived
/// from them. The file committed beside them keeps its history.
#[test]
fn index_stores_no_history_naming_a_secret_like_or_denied_path() {
    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    for dir in ["src/secrets", "tests", "certs"] {
        fs::create_dir_all(repo.join(dir)).unwrap();
    }
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn load_config() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("tests/lib_test.rs"),
        "#[test]\nfn loads() {\n    assert_eq!(1, 1);\n}\n",
    )
    .unwrap();
    fs::write(repo.join(".env"), "TOKEN=fixture\n").unwrap();
    fs::write(repo.join("certs/tls.pem"), "fixture\n").unwrap();
    // Denied by the default `[paths] deny` (`**/secrets/**`), not by the secret-like rule.
    fs::write(repo.join("src/secrets/token.rs"), "pub fn token() {}\n").unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.email", "cli@example.com"]);
    git(&repo, &["config", "user.name", "CLI Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "initial"]);
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn load_config() -> u32 {\n    2\n}\n",
    )
    .unwrap();
    fs::write(repo.join(".env"), "TOKEN=fixture2\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "bump"]);
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });

    let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
    let text_of = |table: &str| -> Vec<String> {
        let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut values = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                match row.get_ref(column).unwrap() {
                    rusqlite::types::ValueRef::Text(text)
                    | rusqlite::types::ValueRef::Blob(text) => {
                        values.push(String::from_utf8_lossy(text).into_owned())
                    }
                    _ => {}
                }
            }
        }
        values
    };
    for table in [
        "git_commits",
        "git_file_touches",
        "git_symbol_touches",
        "git_cochange_edges",
        "history_hotspots",
        "analysis_facts",
        "graph_nodes",
        "graph_edges",
        "graph_strings",
    ] {
        for value in text_of(table) {
            for withheld in [".env", "tls.pem", "secrets/token.rs"] {
                assert!(
                    !value.contains(withheld),
                    "{table} names {withheld}: {value}"
                );
            }
        }
    }
    let touches = text_of("git_file_touches");
    assert!(
        touches
            .iter()
            .filter(|value| value.contains("src/lib.rs"))
            .count()
            >= 2,
        "{touches:?}"
    );
    let edges = text_of("git_cochange_edges");
    assert!(
        edges
            .iter()
            .any(|value| value.contains("tests/lib_test.rs")),
        "{edges:?}"
    );

    let status = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("status")
            .arg("--full");
        command
    });
    // Withheld history is counted in a quality note, never named there. (Discovery's own
    // skipped-path list still names the denied file, as it did before.)
    assert!(status.contains("security policy excludes"), "{status}");
}

#[test]
fn verify_git_checks_both_sides_of_a_rename() {
    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src/secrets")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn load_config() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    let keys = "pub fn signing_key() -> &'static str {\n    \"fixture\"\n}\n\npub fn verifying_key() -> &'static str {\n    \"fixture-public\"\n}\n\npub fn key_id() -> u32 {\n    7\n}\n";
    fs::write(repo.join("src/secrets/keys.rs"), keys).unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.email", "cli@example.com"]);
    git(&repo, &["config", "user.name", "CLI Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    // Verification must pair both sides of the rename without relying on this setting, and
    // without local path prefixes turning one side into two paths or forced colour hiding them.
    git(&repo, &["config", "diff.renames", "false"]);
    git(&repo, &["config", "diff.mnemonicPrefix", "true"]);
    git(&repo, &["config", "diff.srcPrefix", "old/"]);
    git(&repo, &["config", "diff.dstPrefix", "new/"]);
    git(&repo, &["config", "color.diff", "always"]);
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "--quiet", "-m", "initial"]);
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("plan")
            .arg("signing key")
            .arg("--limit")
            .arg("5");
        command
    });
    let mut plan: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let boundary = &mut plan["recommended_change_boundary"];
    boundary["allowed_files"] = serde_json::json!(["src/lib.rs", "src/keys.rs"]);
    boundary["caution_files"] = serde_json::json!([]);
    boundary["caution_rules"] = serde_json::json!([]);
    boundary["forbidden_files"] = serde_json::json!([]);
    boundary["forbidden_rules"] =
        serde_json::json!([{"pattern": "src/secrets/**", "reason": "secrets stay in place"}]);
    let plan_path = temp.path().join("plan.json");
    fs::write(&plan_path, serde_json::to_string(&plan).unwrap()).unwrap();

    git(&repo, &["mv", "src/secrets/keys.rs", "src/keys.rs"]);
    // An edit alongside the move makes git write `---`/`+++` lines, which carry the prefixes.
    fs::write(repo.join("src/keys.rs"), keys.replace("    7\n", "    8\n")).unwrap();

    let assert_both_sides_checked = |report: &serde_json::Value| {
        assert_eq!(report["verdict"], "fail", "{report}");
        assert_eq!(
            report["previous_paths"],
            serde_json::json!([{
                "path": "src/keys.rs",
                "previous_path": "src/secrets/keys.rs",
                "kind": "rename"
            }]),
            "{report}"
        );
        assert_eq!(
            report["changed_files"],
            serde_json::json!(["src/keys.rs", "src/secrets/keys.rs"]),
            "{report}"
        );
        let violation = report["boundary_violations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|finding| {
                finding["kind"] == "forbidden_boundary" && finding["path"] == "src/secrets/keys.rs"
            })
            .unwrap_or_else(|| {
                panic!("the previous path is not held to the forbidden rule: {report}")
            });
        assert!(
            violation["reason"]
                .as_str()
                .unwrap()
                .contains("renamed to `src/keys.rs`"),
            "{violation}"
        );
    };

    let (stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--git");
        command
    });
    assert!(stderr.contains("change verification failed"), "{stderr}");
    assert_both_sides_checked(&serde_json::from_str(&stdout).unwrap());

    let mcp_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {"plan": plan, "since_plan": "HEAD"}
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_request + "\n"),
    );
    let response: serde_json::Value = serde_json::from_str(mcp_verify.trim()).unwrap();
    assert_both_sides_checked(&response["result"]["structuredContent"]);

    let impact = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("impact")
            .arg("--since")
            .arg("HEAD");
        command
    });
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(
        impact["changed_files"][0]["old_path"], "src/secrets/keys.rs",
        "{impact}"
    );
    assert_eq!(
        impact["changed_files"][0]["new_path"], "src/keys.rs",
        "{impact}"
    );
    assert_eq!(
        impact["impact_reports"].as_array().unwrap().len(),
        2,
        "a rename is analysed at its new and its previous path: {impact}"
    );
}

#[test]
fn architecture_policy_bench_scores_checked_in_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("benchmarks/architecture-policy-fixture");
    let cases = root.join("benchmarks/architecture-policy-cases.json");
    let _ = fs::remove_dir_all(fixture.join(".ok"));

    let report = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("architecture")
            .arg("bench")
            .arg(&fixture)
            .arg("--cases-file")
            .arg(&cases)
            .arg("--min-precision")
            .arg("0.95")
            .arg("--min-recall")
            .arg("0.90");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["case_count"], 8);
    assert_eq!(report["summary"]["precision"], 1.0);
    assert_eq!(report["summary"]["recall"], 1.0);
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|case| case["id"] == "dependency-forbidden-call" && case["passed"] == true));
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "public_api_rule" && family["recall"] == 1.0));
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "internal_only_rule" && family["recall"] == 1.0));

    let _ = fs::remove_dir_all(fixture.join(".ok"));
}

#[test]
fn contract_bench_scores_checked_in_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("benchmarks/contract-fixture");
    let cases = root.join("benchmarks/contract-cases.json");

    let report = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("contract-bench")
            .arg(&fixture)
            .arg("--cases-file")
            .arg(&cases)
            .arg("--min-cases")
            .arg("7")
            .arg("--min-verdict-accuracy")
            .arg("0.95")
            .arg("--min-verification-precision")
            .arg("0.95")
            .arg("--min-boundary-precision")
            .arg("0.97")
            .arg("--min-boundary-recall")
            .arg("0.90")
            .arg("--min-toon-reduction")
            .arg("0.35");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["case_count"], 7);
    assert_eq!(report["summary"]["verdict_accuracy"], 1.0);
    assert_eq!(report["summary"]["verification_precision"], 1.0);
    assert!(report["summary"]["min_toon_reduction"].as_f64().unwrap() >= 0.35);
    assert!(report["failures"].as_array().unwrap().is_empty());
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "api_surface_delta"
            && family["verdict_accuracy"] == 1.0));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|case| case["id"] == "contract-dependency-delta" && case["passed"] == true));
}

#[test]
fn init_index_search_and_doctor_work_together() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )
    .unwrap();

    let init = run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    assert!(init.contains("Open Kioku is ready"));
    assert!(repo.join("ok.toml").exists());

    let index = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(index.contains("Indexed"));

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    assert!(status.contains("\"file_count\""));

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker");
        command
    });
    assert!(search.contains("src/lib.rs"));

    let doctor = run({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(doctor.contains("Open Kioku doctor"));
    assert!(doctor.contains("[ok]   repo"));
    assert!(doctor.contains("[ok]   index"));

    let status_markdown_path = repo.join("ok-status.md");
    let status_markdown = run({
        let mut command = ok();
        command
            .arg("status")
            .arg(repo)
            .arg("--markdown")
            .arg("--write")
            .arg(&status_markdown_path);
        command
    });
    assert!(status_markdown.contains("Wrote Open Kioku status snapshot"));
    let written_status = fs::read_to_string(&status_markdown_path).unwrap();
    assert!(written_status.contains("# Open Kioku Status"));
    assert!(written_status.contains("## Readiness Checks"));

    let setup_audit = run({
        let mut command = ok();
        command.arg("setup").arg("audit").arg(repo);
        command
    });
    assert!(setup_audit.contains("Open Kioku setup audit"));
    assert!(setup_audit.contains("MCP clients"));
    assert!(setup_audit.contains("Quality signals"));
    assert!(setup_audit.contains("Advanced providers (optional)"));
    assert!(setup_audit.contains("ok mcp install codex"));

    let setup_markdown = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("audit")
            .arg(repo)
            .arg("--markdown");
        command
    });
    assert!(setup_markdown.contains("# Open Kioku Setup Audit"));
    assert!(setup_markdown.contains("## MCP Client Matrix"));
    assert!(setup_markdown.contains("## Quality Signals"));
    assert!(setup_markdown.contains("## Advanced Providers"));
    assert!(!setup_markdown.contains("codeql CLI/database not detected"));
    assert!(!setup_markdown.contains("0 BSP descriptor"));
}

/// Commit everything in `repo` except Open Kioku's local state, creating the repository on
/// first use. A snapshot import relates the artifact's commit to `HEAD`, so a repository whose
/// snapshot is imported has to be one.
fn commit_all(repo: &std::path::Path, message: &str) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.email=cli@example.com",
                "-c",
                "user.name=CLI Test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    };
    if !repo.join(".git").exists() {
        git(&["init", "--quiet"]);
        fs::write(repo.join(".gitignore"), ".ok/\n").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", message]);
}

fn snapshot_fixture_repo() -> tempfile::TempDir {
    snapshot_fixture_repo_with(&[(
        "src/lib.rs",
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )])
}

fn snapshot_fixture_repo_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    for (path, content) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    commit_all(repo, "initial");
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    temp
}

fn export_snapshot(repo: &std::path::Path) {
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["snapshot", "export", "--quality", "fast"]);
        command
    });
}

fn import_snapshot_json(repo: &std::path::Path, extra: &[&str]) -> serde_json::Value {
    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["--json", "snapshot", "import"])
            .args(extra);
        command
    });
    serde_json::from_str(&output).unwrap()
}

fn status_json(repo: &std::path::Path) -> serde_json::Value {
    let output = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).args(["--json", "status"]);
        command
    });
    serde_json::from_str(&output).unwrap()
}

#[test]
fn snapshot_import_of_the_checked_out_commit_is_fresh() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    export_snapshot(repo);
    // Untracked and not ignored, but under a directory discovery prunes: not a difference
    // between the index and the checkout.
    fs::create_dir_all(repo.join("node_modules/dep")).unwrap();
    fs::write(
        repo.join("node_modules/dep/index.js"),
        "module.exports = 1;\n",
    )
    .unwrap();

    let imported = import_snapshot_json(repo, &[]);
    let snapshot = &imported["snapshot"];
    assert_eq!(snapshot["relation"], "same_commit", "{imported}");
    assert_eq!(snapshot["commits_behind"], 0);
    assert_eq!(snapshot["changed_files"], 0, "{imported}");
    assert_eq!(snapshot["policy_filtered"], 0, "{imported}");
    assert_eq!(imported["caveats"], serde_json::json!([]), "{imported}");

    let status = status_json(repo);
    assert_eq!(status["snapshot"]["relation"], "same_commit", "{status}");

    // `ok index` rebuilds from source and publishes a manifest with no snapshot record.
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(status_json(repo).get("snapshot").is_none());
}

#[test]
fn snapshot_import_of_an_ancestor_commit_is_stale_and_every_surface_says_so() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    export_snapshot(repo);
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} pub fn stop(&self) {} }\n",
    )
    .unwrap();
    commit_all(repo, "add stop");

    let imported = import_snapshot_json(repo, &[]);
    let snapshot = &imported["snapshot"];
    assert_eq!(snapshot["relation"], "related", "{imported}");
    assert_eq!(snapshot["commits_behind"], 1);
    assert_eq!(snapshot["commits_ahead"], 0);
    assert_eq!(snapshot["changed_files"], 1);
    let caveat = imported["caveats"][0].as_str().unwrap().to_string();
    assert!(caveat.contains("1 commit(s) behind"), "{caveat}");

    let status = status_json(repo);
    assert_eq!(status["snapshot"]["commits_behind"], 1, "{status}");
    let repo_status = mcp_repo_status(repo);
    assert_eq!(
        repo_status["result"]["structuredContent"]["snapshot"]["commits_behind"], 1,
        "{repo_status}"
    );

    let doctor: serde_json::Value = serde_json::from_str(&run({
        let mut command = ok();
        command.arg("--json").arg("doctor").arg(repo);
        command
    }))
    .unwrap();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "snapshot")
        .unwrap_or_else(|| panic!("no snapshot check: {doctor}"));
    assert_eq!(check["status"], "warn", "{check}");

    let pack: serde_json::Value = serde_json::from_str(&run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["--json", "context", "Worker run"]);
        command
    }))
    .unwrap();
    assert!(
        pack["retrieval_diagnostics"]["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str() == Some(caveat.as_str())),
        "{pack}"
    );
}

#[test]
fn snapshot_import_refuses_an_artifact_this_repository_cannot_relate_to_head() {
    let exporter = snapshot_fixture_repo();
    export_snapshot(exporter.path());
    let importer = snapshot_fixture_repo_with(&[("src/lib.rs", "pub struct Other;\n")]);
    let repo = importer.path();
    fs::create_dir_all(repo.join(".ok/artifacts")).unwrap();
    for name in ["index.snapshot.zst", "index.snapshot.json"] {
        fs::copy(
            exporter.path().join(".ok/artifacts").join(name),
            repo.join(".ok/artifacts").join(name),
        )
        .unwrap();
    }
    let original_index = fs::read(repo.join(".ok/index.sqlite")).unwrap();
    let import_failure = || {
        run_failure({
            let mut command = ok();
            command.arg("--repo").arg(repo).args(["snapshot", "import"]);
            command
        })
        .1
    };

    // The artifact's commit is not an object here.
    let stderr = import_failure();
    assert!(stderr.contains("is not in this repository"), "{stderr}");
    assert!(stderr.contains("--allow-foreign"), "{stderr}");
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );

    // Fetched, it is an object here with no history in common with HEAD.
    let fetched = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["fetch", "--quiet"])
        .arg(exporter.path())
        .arg("HEAD")
        .status()
        .unwrap();
    assert!(fetched.success());
    let stderr = import_failure();
    assert!(stderr.contains("shares no history with HEAD"), "{stderr}");
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );

    // `--from-snapshot auto` applies the same refusal and indexes from source instead.
    let (stdout, stderr) = run_ok_with_stderr({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["--json", "index", "--from-snapshot", "auto"]);
        command
    });
    assert!(stderr.contains("falling back to full index"), "{stderr}");
    let manifest: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(manifest.get("imported").is_none(), "{manifest}");
    assert!(manifest.get("snapshot").is_none(), "{manifest}");

    let imported = import_snapshot_json(repo, &["--allow-foreign"]);
    assert_eq!(imported["snapshot"]["relation"], "foreign", "{imported}");
    assert!(imported["caveats"][0]
        .as_str()
        .unwrap()
        .contains("--allow-foreign"));
    assert_eq!(status_json(repo)["snapshot"]["relation"], "foreign");
}

/// FNV-1a over `value`, as the graph dictionaries key their entries.
fn graph_string_hash(value: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

/// Replace `from` with `to` in every text column of every table, keeping column and JSON in
/// step and re-keying the graph dictionaries, so the result is what a consistent writer that
/// had indexed the file under `to` would have stored.
fn rename_everywhere(db: &std::path::Path, from: &str, to: &str) {
    let conn = rusqlite::Connection::open(db).unwrap();
    let tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect::<Vec<_>>();
    for table in tables {
        let columns = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .filter(|(_, kind)| kind.eq_ignore_ascii_case("TEXT"))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        for column in columns {
            conn.execute(
                &format!(
                    "UPDATE {table} SET {column} = replace({column}, ?1, ?2) \
                     WHERE {column} LIKE '%' || ?1 || '%'"
                ),
                [from, to],
            )
            .unwrap();
        }
    }
    for table in ["graph_strings", "call_site_strings"] {
        let rows = conn
            .prepare(&format!("SELECT sid, value FROM {table}"))
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        for (sid, value) in rows {
            conn.execute(
                &format!("UPDATE {table} SET vhash = ?1 WHERE sid = ?2"),
                rusqlite::params![graph_string_hash(&value), sid],
            )
            .unwrap();
        }
    }
}

#[test]
fn snapshot_import_serves_no_path_the_local_policy_excludes() {
    let temp = snapshot_fixture_repo_with(&[
        ("src/lib.rs", "pub struct Worker;\n"),
        ("src/vault.rs", "pub struct VaultWidget;\n"),
        ("legacy/old.rs", "pub struct LegacyWidget;\n"),
    ]);
    let repo = temp.path();
    // An exporter that indexed a secret-like path: an older release, another policy, or a
    // crafted artifact. Every stored mention of `src/vault.rs` becomes `deploy/vault.key`, in
    // every table, graph included; the graph dictionary is re-keyed so it stays consistent.
    rename_everywhere(
        &repo.join(".ok/index.sqlite"),
        "src/vault.rs",
        "deploy/vault.key",
    );
    export_snapshot(repo);
    // The importing checkout excludes `legacy/`; the exporter did not.
    let config = fs::read_to_string(repo.join("ok.toml")).unwrap();
    assert!(config.contains("exclude = [\n"));
    fs::write(
        repo.join("ok.toml"),
        config.replacen("exclude = [\n", "exclude = [\n    \"legacy/**\",\n", 1),
    )
    .unwrap();

    let imported = import_snapshot_json(repo, &[]);
    assert_eq!(imported["snapshot"]["policy_filtered"], 2, "{imported}");
    assert_eq!(
        imported["policy_filtered_by_source"],
        serde_json::json!({"security_policy": 1, "config_exclude": 1}),
        "{imported}"
    );
    assert_eq!(status_json(repo)["snapshot"]["policy_filtered"], 2);

    for (query, path) in [
        ("VaultWidget", "vault.key"),
        ("LegacyWidget", "legacy/old.rs"),
    ] {
        let search = run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .args(["--json", "search", query]);
            command
        });
        assert!(!search.contains(path), "{query}: {search}");
    }
    // No row of the published database names either path.
    let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
    for table in [
        "files",
        "chunks",
        "symbols",
        "graph_nodes",
        "document_sections",
    ] {
        let json_column = if table == "document_sections" {
            "path"
        } else {
            "json"
        };
        let count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE {json_column} LIKE '%vault.key%' \
                     OR {json_column} LIKE '%legacy/old.rs%'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table} still names an excluded path");
    }
    for (table, column) in [
        ("graph_strings", "value"),
        ("graph_nodes", "id"),
        ("analysis_facts", "target"),
        ("git_symbol_touches", "file_path"),
    ] {
        let count: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM {table} WHERE {column} LIKE '%vault%' \
                     OR {column} LIKE '%legacy/old.rs%'"
                ),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "{table} still names an excluded path");
    }
    // Git history names a secret-like path nowhere. File-level history of a merely excluded
    // path is kept, as `ok index` records history for every path a commit touched.
    let history_rows = |pattern: &str| -> i64 {
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM git_file_touches WHERE path LIKE ?1) \
                  + (SELECT COUNT(*) FROM git_cochange_edges \
                     WHERE path LIKE ?1 OR cochanged_path LIKE ?1)",
            [pattern],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert_eq!(history_rows("%vault%"), 0);
    assert!(history_rows("legacy/old.rs") > 0);
    drop(conn);

    // The coverage the index reports agrees with what it serves: both files are counted as
    // excluded by the rule that excluded them, and the secret-like one is not named.
    let status = status_json(repo);
    let excluded = &status["coverage"]["policy_excluded_by_source"];
    assert_eq!(excluded["security_policy"], 1, "{status}");
    assert_eq!(excluded["config_exclude"], 1, "{status}");
    assert!(!status.to_string().contains("vault.key"), "{status}");
}

/// `ok watch` replaces the rows of the files that changed. A deleted file's co-change facts
/// held by an unchanged file used to survive it, so the next export carried a fact about a
/// file the index no longer had, and the import refused it as inconsistent.
#[test]
fn snapshot_of_a_watched_index_after_a_co_changed_file_is_deleted_imports() {
    let temp = snapshot_fixture_repo_with(&[
        ("src/lib.rs", "pub mod a;\n"),
        ("src/a.rs", "pub fn ledger_alpha() {}\n"),
        ("src/d.rs", "pub fn ledger_delta() {}\n"),
    ]);
    let repo = temp.path();
    let facts_about_deleted = || -> i64 {
        rusqlite::Connection::open(repo.join(".ok/index.sqlite"))
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM analysis_facts WHERE target = 'src/d.rs'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(
        facts_about_deleted() > 0,
        "the fixture must record a co-change fact about src/d.rs"
    );

    fs::remove_file(repo.join("src/d.rs")).unwrap();
    let status =
        open_kioku_watch::reindex_repo_after_changes(repo, [std::path::Path::new("src/d.rs")])
            .unwrap();
    assert!(
        status.partial && status.deleted_files == 1 && status.changed_files == 0,
        "the watch update must be the partial one: {status:?}"
    );
    assert_eq!(
        facts_about_deleted(),
        0,
        "facts about a deleted file must go with it"
    );

    export_snapshot(repo);
    let imported = import_snapshot_json(repo, &[]);
    assert_eq!(imported["imported"], true, "{imported}");
}

/// A protobuf length-delimited field.
fn protobuf_field(number: u32, bytes: &[u8]) -> Vec<u8> {
    fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }
    let mut out = Vec::new();
    varint(u64::from(number << 3 | 2), &mut out);
    varint(bytes.len() as u64, &mut out);
    out.extend_from_slice(bytes);
    out
}

/// A SCIP index with one document, for a file discovery skips, holding one symbol and its
/// definition. Written by hand so the test needs no SCIP generator.
fn scip_index_for(relative_path: &str, symbol: &str) -> Vec<u8> {
    let mut information = protobuf_field(1, symbol.as_bytes());
    information.extend(protobuf_field(6, b"GeneratedApi"));
    let mut occurrence = protobuf_field(1, &[0, 11, 23]);
    occurrence.extend(protobuf_field(2, symbol.as_bytes()));
    occurrence.extend([3 << 3, 1]); // symbol_roles: Definition
    let mut document = protobuf_field(1, relative_path.as_bytes());
    document.extend(protobuf_field(2, &occurrence));
    document.extend(protobuf_field(3, &information));
    document.extend(protobuf_field(4, b"rust"));
    protobuf_field(2, &document)
}

/// SCIP covers every document it was generated for, including files discovery skipped, and
/// `ok index` stores those symbols with a `file_id` no file row has. That is a state the
/// writer produces, so the import's consistency check accepts it, and the rows serve no path.
#[test]
fn snapshot_import_accepts_scip_symbols_for_a_file_discovery_skipped() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub struct Worker;\n").unwrap();
    fs::write(
        repo.join("index.scip"),
        scip_index_for(
            "generated/api.rs",
            "rust-analyzer cargo fixture 0.1.0 generated/api/GeneratedApi#",
        ),
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    commit_all(repo, "initial");
    // Generated after the first commit and ignored, so discovery skips it and it is untracked.
    fs::write(repo.join(".gitignore"), ".ok/\ngenerated/\n").unwrap();
    commit_all(repo, "ignore generated sources");
    fs::create_dir_all(repo.join("generated")).unwrap();
    fs::write(repo.join("generated/api.rs"), "pub struct GeneratedApi;\n").unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    {
        let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
        let orphaned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM symbols WHERE file_id NOT IN (SELECT id FROM files) \
                 AND json_extract(json, '$.provenance') = 'scip'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            orphaned > 0,
            "the fixture must store a SCIP symbol for an unindexed file"
        );
    }
    export_snapshot(repo);

    let imported = import_snapshot_json(repo, &[]);
    assert_eq!(
        imported["snapshot"]["relation"], "same_commit",
        "{imported}"
    );
    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["--json", "search", "GeneratedApi"]);
        command
    });
    assert!(!search.contains("generated/api.rs"), "{search}");
}

/// The policy is decided on each row's path column, and readers serve the path in its JSON.
/// An artifact whose two disagree, or whose graph dictionary is keyed wrongly, is refused
/// before anything is replaced: judged by one path and served under another, a secret-like
/// path would pass the policy.
#[test]
fn snapshot_import_refuses_rows_whose_served_path_differs_from_their_column() {
    let cases: [(&str, &str); 3] = [
        (
            "files",
            "UPDATE files SET json = json_set(json, '$.path', '.env') WHERE path = 'src/lib.rs'",
        ),
        (
            "file history",
            "UPDATE git_file_touches SET json = json_set(json, '$.path', 'deploy/id_rsa') \
             WHERE path = 'src/lib.rs'",
        ),
        (
            "graph dictionary",
            "UPDATE graph_strings SET value = 'deploy/id_rsa' WHERE value = 'src/lib.rs'",
        ),
    ];
    for (name, tamper) in cases {
        let temp = snapshot_fixture_repo();
        let repo = temp.path();
        export_snapshot(repo);
        // Tamper with the artifact itself: expand it, change one row, compress it again and
        // restate its sizes, so every structural check still passes.
        let artifacts = repo.join(".ok/artifacts");
        let db = artifacts.join("tampered.sqlite");
        let compressed = fs::read(artifacts.join("index.snapshot.zst")).unwrap();
        fs::write(&db, zstd::decode_all(compressed.as_slice()).unwrap()).unwrap();
        let changed = rusqlite::Connection::open(&db)
            .unwrap()
            .execute(tamper, [])
            .unwrap();
        assert!(changed > 0, "{name}: the tamper must change a row");
        let raw = fs::read(&db).unwrap();
        let recompressed = zstd::encode_all(raw.as_slice(), 1).unwrap();
        fs::write(artifacts.join("index.snapshot.zst"), &recompressed).unwrap();
        let metadata_path = artifacts.join("index.snapshot.json");
        let mut metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
        metadata["original_size_bytes"] = raw.len().into();
        metadata["compressed_size_bytes"] = recompressed.len().into();
        fs::write(&metadata_path, metadata.to_string()).unwrap();
        fs::remove_file(&db).unwrap();

        let original_index = fs::read(repo.join(".ok/index.sqlite")).unwrap();
        let (_, stderr) = run_failure({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .args(["snapshot", "import", "--allow-foreign"]);
            command
        });
        assert!(stderr.contains("rows are inconsistent"), "{name}: {stderr}");
        assert_eq!(
            fs::read(repo.join(".ok/index.sqlite")).unwrap(),
            original_index,
            "{name}: the current index must be left in place"
        );
        let leftovers = fs::read_dir(&artifacts)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            leftovers.is_empty(),
            "{name}: staged files left: {leftovers:?}"
        );
    }
}

#[test]
fn fresh_indexes_of_one_tree_agree_on_context_order_and_quality_notes() {
    // Every widget file matches the task identically, so their search scores tie and only the
    // tie-break decides their order. The second copy is written in reverse because some
    // filesystems list directory entries in creation order.
    let copies = [
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    ];
    for (copy_index, copy) in copies.iter().enumerate() {
        let repo = copy.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        let mut order = (0..16).collect::<Vec<usize>>();
        if copy_index == 1 {
            order.reverse();
        }
        for index in order {
            fs::write(
                repo.join(format!("src/widget_{index:02}.rs")),
                format!(
                    "pub fn render_widget_{index:02}(frame: u32) -> u32 {{\n    absent_alpha_{index:02}();\n    absent_bravo_{index:02}();\n    absent_charlie_{index:02}();\n    absent_delta_{index:02}();\n    absent_echo_{index:02}();\n    frame\n}}\n"
                ),
            )
            .unwrap();
        }
    }

    let observe = |repo: &std::path::Path| {
        run({
            let mut command = ok();
            command.arg("init").arg(repo);
            command
        });
        run({
            let mut command = ok();
            command.env("RAYON_NUM_THREADS", "4").arg("index").arg(repo);
            command
        });
        let pack: serde_json::Value = serde_json::from_str(&run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .arg("--json")
                .arg("context")
                .arg("render widget frame");
            command
        }))
        .unwrap();
        let paths = ["primary_files", "supporting_files"]
            .iter()
            .flat_map(|key| pack[*key].as_array().cloned().unwrap_or_default())
            .map(|result| format!("{} {}", result["path"], result["line_range"]))
            .collect::<Vec<_>>();
        let status: serde_json::Value = serde_json::from_str(&run({
            let mut command = ok();
            command.arg("--json").arg("status").arg(repo).arg("--full");
            command
        }))
        .unwrap();
        let mut notes = status["quality"]["quality_notes"].to_string();
        for root in [repo.to_path_buf(), repo.canonicalize().unwrap()] {
            notes = notes.replace(&root.display().to_string(), "<root>");
        }
        (paths, notes)
    };

    let (baseline_paths, baseline_notes) = observe(copies[0].path());
    assert!(
        baseline_paths.len() >= 2,
        "the task must select several tied files: {baseline_paths:?}"
    );
    assert!(baseline_notes.contains("symbol registry unresolved"));
    for copy in &copies[1..] {
        let (paths, notes) = observe(copy.path());
        assert_eq!(paths, baseline_paths);
        assert_eq!(notes, baseline_notes);
    }
}

/// Set the marker `SqliteStore::open` leaves behind when it discards a pre-4.0 edge layout.
fn mark_graph_rebuild_required(repo: &std::path::Path) {
    let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); \
         INSERT OR REPLACE INTO schema_meta(key, value) VALUES('graph_rebuild_required_v4', '1');",
    )
    .unwrap();
}

/// The analysis fingerprint is unchanged since 3.1.0, so only the marker records that a
/// pre-4.0 index's edges were discarded on open. Impact, plan and context name the rebuild
/// instead of answering with empty relationship lists, and every status surface reports the
/// marker.
#[test]
fn impact_plan_context_and_status_surfaces_report_a_graph_awaiting_a_rebuild() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    mark_graph_rebuild_required(repo);

    for args in [
        vec!["impact", "--file", "src/lib.rs"],
        vec!["plan", "change Worker::run"],
        vec!["preflight", "change Worker::run"],
        // The task must select a primary file; a pack with none never reads the graph.
        vec!["context", "change Worker::run"],
    ] {
        let (_stdout, stderr) = run_failure({
            let mut command = ok();
            command.arg("--repo").arg(repo).args(&args);
            command
        });
        assert!(
            stderr.contains("older index format") && stderr.contains("ok index"),
            "{args:?} must name the rebuild, got: {stderr}"
        );
    }

    let (doctor, _stderr) = run_failure({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(
        doctor.contains("[fail] graph") && doctor.contains("run `ok index`"),
        "{doctor}"
    );
    assert!(
        doctor.contains("[ok]   index"),
        "file and symbol counts are intact: {doctor}"
    );

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["graph_rebuild_required"], true);
    assert_eq!(
        status["analysis_semantics_status"]["status"], "compatible",
        "the fingerprint alone does not see the marker: {status}"
    );

    let status_text = run({
        let mut command = ok();
        command.arg("status").arg(repo);
        command
    });
    assert!(
        status_text.contains("graph awaiting rebuild: run `ok index`"),
        "{status_text}"
    );

    let audit = run({
        let mut command = ok();
        command.arg("--json").arg("setup").arg("audit").arg(repo);
        command
    });
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["ok"], false);
    let graph = audit["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "graph")
        .expect("setup audit carries the graph check");
    assert_eq!(graph["status"], "fail");
    assert!(graph["message"]
        .as_str()
        .unwrap()
        .contains("run `ok index`"));
}

/// A store whose edges were discarded still has an edge table, so the export would happily
/// count it and publish `graph_edge_count: 0` as though the repository had been measured.
#[test]
fn snapshot_export_refuses_a_store_whose_graph_awaits_a_rebuild() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    mark_graph_rebuild_required(repo);

    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg("fast");
        command
    });
    assert!(
        stderr.contains("older index format") && stderr.contains("ok index"),
        "expected a rebuild instruction, got: {stderr}"
    );
    assert!(
        !repo.join(".ok/artifacts/index.snapshot.json").exists(),
        "metadata must not be written for a store that cannot be measured"
    );
}

#[test]
fn snapshot_export_import_round_trip_rebuilds_search_and_bootstraps_index() {
    // `best` rebuilds the database with `VACUUM INTO`; `fast` copies its pages with the online
    // backup API. An artifact of either quality must import, serve search and bootstrap.
    for (quality, compression_level) in [("best", 9), ("fast", 1)] {
        assert_snapshot_round_trip(quality, compression_level);
    }
}

fn assert_snapshot_round_trip(quality: &str, compression_level: i64) {
    // A relative TypeScript import and an HTTP route give the graph nodes no file owns whose
    // labels are not repository paths (`../utils/foo`, `/api/users`); Git rejects both, so
    // the import must judge them without asking it.
    let temp = snapshot_fixture_repo_with(&[
        (
            "src/lib.rs",
            "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
        ),
        (
            "web/src/app/main.ts",
            "import express from \"express\";\nimport { foo } from \"../utils/foo\";\n\
             const app = express();\napp.get(\"/api/users\", (req, res) => res.json(foo()));\n",
        ),
        (
            "web/src/utils/foo.ts",
            "export function foo() { return []; }\n",
        ),
    ]);
    let repo = temp.path();
    {
        let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
        let unowned_labels = conn
            .prepare("SELECT label FROM graph_nodes WHERE file_id IS NULL OR file_id = ''")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert!(
            unowned_labels.iter().any(|label| label.starts_with("../")),
            "the fixture must produce a relative import node: {unowned_labels:?}"
        );
        assert!(
            unowned_labels
                .iter()
                .any(|label| label.contains("/api/users")),
            "the fixture must produce a route node: {unowned_labels:?}"
        );
    }
    let artifact_path = repo.join(".ok/artifacts/index.snapshot.zst");
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");
    let gitattributes_path = repo.join(".ok/artifacts/.gitattributes");
    let search_meta = repo.join(".ok/search/tantivy/meta.json");

    let exported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg(quality);
        command
    });
    let exported: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(exported["ok"], true);
    assert_eq!(exported["quality"], quality);
    assert_eq!(exported["metadata"]["artifact_kind"], "index-snapshot");
    assert_eq!(exported["metadata"]["schema_version"], "1.0.0");
    assert_eq!(exported["metadata"]["compression_level"], compression_level);
    assert!(exported["metadata"]["file_count"].as_u64().unwrap() >= 1);
    assert!(exported["metadata"]["chunk_count"].as_u64().unwrap() >= 1);
    assert!(artifact_path.exists());
    assert!(metadata_path.exists());
    assert!(gitattributes_path.exists());
    assert!(fs::read_to_string(&gitattributes_path)
        .unwrap()
        .contains("*.snapshot.zst binary -merge"));

    fs::write(repo.join(".ok/memory.sqlite"), b"private memory").unwrap();
    fs::write(repo.join(".ok/context.sqlite"), b"private context").unwrap();
    fs::remove_file(repo.join(".ok/index.sqlite")).unwrap();
    fs::remove_dir_all(repo.join(".ok/search")).unwrap();
    fs::remove_file(repo.join(".ok/memory.sqlite")).unwrap();
    fs::remove_file(repo.join(".ok/context.sqlite")).unwrap();
    fs::write(repo.join(".ok/index.sqlite-wal"), b"stale wal").unwrap();
    fs::write(repo.join(".ok/index.sqlite-shm"), b"stale shm").unwrap();

    let imported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("import");
        command
    });
    let imported: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(imported["ok"], true);
    assert_eq!(imported["imported"], true);
    assert_eq!(imported["rebuilt_search"], true);
    assert!(repo.join(".ok/index.sqlite").exists());
    assert!(search_meta.exists());
    assert!(!repo.join(".ok/index.sqlite-wal").exists());
    assert!(!repo.join(".ok/index.sqlite-shm").exists());
    assert!(!repo.join(".ok/memory.sqlite").exists());
    assert!(!repo.join(".ok/context.sqlite").exists());

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker");
        command
    });
    assert!(search.contains("src/lib.rs"));

    let graph_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker")
            .arg("--kind")
            .arg("graph");
        command
    });
    assert!(graph_search.contains("Worker"));

    let doctor = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("doctor");
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    assert_eq!(doctor["ok"], true);

    fs::remove_file(repo.join(".ok/index.sqlite")).unwrap();
    fs::remove_dir_all(repo.join(".ok/search")).unwrap();
    let bootstrapped = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("index")
            .arg("--from-snapshot")
            .arg("auto");
        command
    });
    let bootstrapped: serde_json::Value = serde_json::from_str(&bootstrapped).unwrap();
    assert_eq!(bootstrapped["imported"], true);
    assert!(repo.join(".ok/index.sqlite").exists());
    assert!(search_meta.exists());
}

#[test]
fn snapshot_import_rejects_invalid_artifacts_without_replacing_existing_index() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    let artifact_path = repo.join(".ok/artifacts/index.snapshot.zst");
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg("fast");
        command
    });
    let original_index = fs::read(repo.join(".ok/index.sqlite")).unwrap();
    let original_metadata = fs::read_to_string(&metadata_path).unwrap();
    let original_artifact = fs::read(&artifact_path).unwrap();

    fs::remove_file(&metadata_path).unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("snapshot metadata is missing"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );
    fs::write(&metadata_path, &original_metadata).unwrap();

    fs::write(&artifact_path, b"not a zstd snapshot").unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("snapshot compressed size mismatch"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );
    fs::write(&artifact_path, &original_artifact).unwrap();

    let mut metadata: serde_json::Value = serde_json::from_str(&original_metadata).unwrap();
    metadata["schema_version"] = serde_json::Value::String("9.9.9".into());
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("unsupported snapshot schema version"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );

    let mut metadata: serde_json::Value = serde_json::from_str(&original_metadata).unwrap();
    metadata["open_kioku_version"] = serde_json::Value::String("0.0.0".into());
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let imported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("import");
        command
    });
    let imported: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(imported["ok"], true);
    assert!(imported["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap()
            .contains("snapshot was exported by Open Kioku 0.0.0")));
}

#[test]
fn impact_and_plan_accept_since_changed_ranges() {
    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn token() -> &'static str {\n    \"old\"\n}\n",
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    git(repo, &["init", "--quiet"]);
    git(repo, &["config", "user.email", "cli@example.com"]);
    git(repo, &["config", "user.name", "CLI Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", "initial"]);
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn token() -> &'static str {\n    \"new\"\n}\n",
    )
    .unwrap();

    let impact = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("impact")
            .arg("--since")
            .arg("HEAD");
        command
    });
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(impact["since"], "HEAD");
    assert_eq!(impact["changed_files"][0]["new_path"], "src/lib.rs");
    assert_eq!(
        impact["changed_files"][0]["hunks"][0]["new_range"]["start"],
        2
    );

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("plan")
            .arg("update token")
            .arg("--since")
            .arg("HEAD")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(plan.contains("git diff HEAD --unified=0"));
    assert!(plan.contains("src/lib.rs"));
}

#[test]
fn index_mode_is_reported_by_index_and_status_json() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    fs::write(repo.join("docs/guide.rs"), "pub fn docs_only() {}\n").unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });

    let indexed = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg(repo)
            .arg("--mode")
            .arg("fast");
        command
    });
    let indexed: serde_json::Value = serde_json::from_str(&indexed).unwrap();
    assert_eq!(indexed["index_mode"], "fast");
    assert!(indexed["phase_reports"].as_array().unwrap().len() >= 2);
    assert_eq!(indexed["quality"]["skip_counts"]["fast_mode"], 1);
    assert!(indexed["quality"]["skipped_paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path["reason"] == "fast_mode" && path["source"] == "fast_mode"));
    assert!(indexed["quality"]["quality_notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|note| note["kind"] == "index_mode"
            && note["message"]
                .as_str()
                .unwrap_or_default()
                .contains("fast mode")));

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["index_mode"], "fast");
    assert_eq!(status["quality"]["skip_counts"]["fast_mode"], 1);
    // Status carries counts plus a sample; `--full` carries the manifest's lists.
    assert_eq!(status["quality"]["skipped_paths"]["total"], 1);
    assert_eq!(
        status["quality"]["skipped_paths"]["by_reason"]["fast_mode"],
        1
    );
    assert!(status["quality"]["skipped_paths"]["sample"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path["reason"] == "fast_mode" && path["source"] == "fast_mode"));
    assert_eq!(
        status["quality"]["quality_notes"]["by_kind"]["index_mode"],
        1
    );
    let sample_len = status["quality"]["quality_notes"]["sample"]
        .as_array()
        .unwrap()
        .len();
    assert!(sample_len <= 20, "{sample_len}");
    assert!(
        status["quality"]["quality_notes"]["total"]
            .as_u64()
            .unwrap()
            >= sample_len as u64
    );
    let full = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo).arg("--full");
        command
    });
    let full: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert_eq!(
        full["quality"]["skipped_paths"],
        indexed["quality"]["skipped_paths"]
    );
    assert_eq!(
        full["quality"]["quality_notes"],
        indexed["quality"]["quality_notes"]
    );

    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("index")
            .arg(repo)
            .arg("--mode")
            .arg("unsupported");
        command
    });
    assert!(stderr.contains("unsupported index mode"));
}

#[test]
fn cross_project_workspace_links_existing_project_indexes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("fleet");
    let service_a = temp.path().join("service-a");
    let service_b = temp.path().join("service-b");
    let service_c = temp.path().join("service-c");
    let service_missing = temp.path().join("service-missing");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&service_a).unwrap();
    fs::create_dir_all(&service_b).unwrap();
    fs::create_dir_all(&service_c).unwrap();
    fs::create_dir_all(&service_missing).unwrap();

    fs::write(
        service_a.join("index.ts"),
        r#"
export function register(router: { get: Function }, consumer: { subscribe: Function }) {
  router.get("/v1/orders", () => "ok");
  consumer.subscribe("orders.created", () => {});
}
"#,
    )
    .unwrap();
    fs::write(
        service_b.join("index.ts"),
        r#"
export async function callOrders() {
  return fetch("https://service-a.local/v1/orders");
}

export function publishOrder(producer: { send: Function }) {
  producer.send({ topic: "orders.created" });
}
"#,
    )
    .unwrap();
    fs::write(
        service_c.join("index.ts"),
        r#"
export function register(router: { get: Function }) {
  router.get("/v1/orders", () => "alternate");
}
"#,
    )
    .unwrap();

    for repo in [&service_a, &service_b, &service_c] {
        run({
            let mut command = ok();
            command.arg("init").arg(repo);
            command
        });
        run({
            let mut command = ok();
            command.arg("index").arg(repo);
            command
        });
    }

    let service_a_index = service_a.join(".ok/index.sqlite");
    let service_a_modified = fs::metadata(&service_a_index).unwrap().modified().unwrap();

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
  {{ name = "service-b", repo = "{}" }},
]
"#,
            service_a.display(),
            service_b.display()
        ),
    )
    .unwrap();

    let linked = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let linked: serde_json::Value = serde_json::from_str(&linked).unwrap();
    assert_eq!(linked["project_count"], 2);
    assert_eq!(linked["link_count"], 2);
    assert!(linked["graph_path"]
        .as_str()
        .unwrap()
        .ends_with("workspace.sqlite"));
    assert!(workspace.join(".ok/workspace.sqlite").exists());
    assert_eq!(
        fs::metadata(&service_a_index).unwrap().modified().unwrap(),
        service_a_modified,
        "cross-project indexing must not mutate project indexes"
    );
    assert!(linked["links"].as_array().unwrap().iter().any(|link| {
        link["source_project"] == "service-b"
            && link["target_project"] == "service-a"
            && link["target"] == "/v1/orders"
            && link["edge_type"] == "CALLS_ENDPOINT"
    }));
    assert!(linked["links"].as_array().unwrap().iter().any(|link| {
        link["source_project"] == "service-b"
            && link["target_project"] == "service-a"
            && link["target"] == "orders.created"
            && link["edge_type"] == "PUBLISHES_EVENT"
    }));

    let fleet = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("architecture")
            .arg("fleet")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let fleet: serde_json::Value = serde_json::from_str(&fleet).unwrap();
    assert_eq!(fleet["project_count"], 2);
    assert_eq!(fleet["link_count"], 2);

    fs::write(
        service_a.join("alternate.ts"),
        r#"
export function registerAlternate(router: { get: Function }) {
  router.get("/v1/orders", () => "alternate");
}
"#,
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(&service_a);
        command
    });
    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
  {{ name = "service-b", repo = "{}" }},
  {{ name = "service-c", repo = "{}" }},
]
"#,
            service_a.display(),
            service_b.display(),
            service_c.display()
        ),
    )
    .unwrap();
    let ambiguous = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let ambiguous: serde_json::Value = serde_json::from_str(&ambiguous).unwrap();
    let ambiguous_http_links = ambiguous["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|link| {
            link["source_project"] == "service-b"
                && link["target_project"] == "service-a"
                && link["target"] == "/v1/orders"
        })
        .collect::<Vec<_>>();
    assert_eq!(ambiguous_http_links.len(), 2);
    assert!(ambiguous_http_links.iter().all(|link| {
        link["confidence"] == "medium"
            && link["ambiguity"].as_array().unwrap().iter().any(|note| {
                note.as_str()
                    .unwrap_or_default()
                    .contains("candidate cross-project targets")
            })
    }));

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
]
"#,
            service_a.display()
        ),
    )
    .unwrap();
    let relinked = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let relinked: serde_json::Value = serde_json::from_str(&relinked).unwrap();
    assert_eq!(
        relinked["link_count"], 0,
        "stale workspace edges are removed"
    );

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-missing", repo = "{}" }},
]
"#,
            service_missing.display()
        ),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    assert!(stderr.contains("missing project index"));
}

#[test]
fn mcp_install_prints_client_config() {
    let temp = tempfile::tempdir().unwrap();
    let output = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("claude")
            .arg("--repo")
            .arg(temp.path());
        command
    });

    assert!(output.contains("mcpServers"));
    assert!(output.contains("\"command\": \"ok\""));
    assert!(output.contains("--read-only"));
    assert!(output.contains("apply source edits with your normal editor"));
    assert!(!output.contains("OPEN_KIOKU_ALLOW_WRITE"));

    let codex = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("codex")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(codex.contains("[mcp_servers.open-kioku]"));
    assert!(codex.contains("command = \"ok\""));

    let opencode = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("opencode")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(opencode.contains("\"mcp\""));
    assert!(opencode.contains("\"type\": \"local\""));

    let zed = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("zed")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(zed.contains("context_servers"));
    assert!(zed.contains("open-kioku"));

    let windsurf = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("windsurf")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(windsurf.contains("mcpServers"));
    assert!(windsurf.contains("open-kioku"));

    let trae = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("trae")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(trae.contains("mcpServers"));
    assert!(trae.contains("open-kioku"));
}

#[test]
fn patch_and_mcp_help_expose_only_supported_source_workflow() {
    let patch_help = run({
        let mut command = ok();
        command.arg("patch").arg("--help");
        command
    });
    assert!(patch_help.contains("plan"));
    assert!(!patch_help.contains("review"));
    assert!(!patch_help.contains("apply"));

    let mcp_serve_help = run({
        let mut command = ok();
        command.args(["mcp", "serve", "--help"]);
        command
    });
    assert!(mcp_serve_help.contains("--read-only"));
    assert!(!mcp_serve_help.contains("--allow-write"));
}

fn semantic_json_report(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim()).expect("semantic --json stdout should be one JSON document")
}

/// The semantic vector store, resolved the way the product resolves it. Hardcoding
/// `.ok/vectors` passes until an `ok index` publishes a generation and moves the store under
/// `.ok/generations/<id>/`, which is exactly what this test does between semantic runs.
fn semantic_current_dir(repo: &std::path::Path) -> PathBuf {
    open_kioku_storage::generations::resolve_index_location(repo)
        .vectors_root()
        .join("current")
}

fn semantic_target_keys(repo: &std::path::Path) -> Vec<(String, String, String)> {
    let path = semantic_current_dir(repo).join("ids.json");
    let ids = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {} failed: {err}", path.display()));
    let targets: Vec<serde_json::Value> = serde_json::from_str(&ids).unwrap();
    targets
        .iter()
        .map(|target| {
            (
                target["stable_id"].as_str().unwrap().to_string(),
                target["content_hash"].as_str().unwrap().to_string(),
                target["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// `ok --json semantic <subcommand>`, returning (stdout, stderr).
fn run_semantic(repo: &std::path::Path, subcommand: &str) -> (String, String) {
    run_ok_with_stderr({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("semantic")
            .arg(subcommand);
        command
    })
}

#[test]
fn semantic_index_reports_progress_on_stderr_and_reembeds_only_changed_files() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });
    let semantic = |subcommand: &str| run_semantic(&repo, subcommand);

    let (stdout, stderr) = semantic("index");
    let first = semantic_json_report(&stdout);
    let indexed = first["indexed_count"].as_u64().unwrap();
    assert!(indexed > 0);
    assert_eq!(first["embedded_count"].as_u64(), Some(indexed));
    assert_eq!(first["reused_embeddings"].as_u64(), Some(0));
    // Captured stderr is not a terminal: whole lines, never an in-place redraw.
    assert!(!stderr.contains('\r'), "{stderr}");
    let progress = stderr
        .lines()
        .filter(|line| line.starts_with("semantic[index] "))
        .collect::<Vec<_>>();
    assert!(!progress.is_empty(), "{stderr}");
    assert!(
        progress.iter().all(|line| line.contains("elapsed=")),
        "{stderr}"
    );
    assert!(
        progress
            .last()
            .unwrap()
            .contains(&format!("{indexed}/{indexed} targets embedded")),
        "{stderr}"
    );

    let before = semantic_target_keys(&repo);
    let (stdout, stderr) = semantic("index");
    let unchanged = semantic_json_report(&stdout);
    assert_eq!(unchanged["embedded_count"].as_u64(), Some(0));
    assert_eq!(unchanged["reused_embeddings"].as_u64(), Some(indexed));
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("semantic[index] nothing to embed")),
        "{stderr}"
    );

    let lib = repo.join("src/lib.rs");
    let mut source = fs::read_to_string(&lib).unwrap();
    source.push_str(
        "\npub fn handle_logout(user_id: &str) -> String {\n    format!(\"logout:{user_id}\")\n}\n",
    );
    fs::write(&lib, source).unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });

    let (stdout, _) = semantic("index");
    let changed = semantic_json_report(&stdout);
    let after = semantic_target_keys(&repo);
    let known = before
        .iter()
        .map(|(stable_id, content_hash, _)| (stable_id.clone(), content_hash.clone()))
        .collect::<std::collections::HashSet<_>>();
    let fresh = after
        .iter()
        .filter(|(stable_id, content_hash, _)| {
            !known.contains(&(stable_id.clone(), content_hash.clone()))
        })
        .collect::<Vec<_>>();
    assert!(!fresh.is_empty());
    assert!(
        fresh.iter().all(|(_, _, path)| path == "src/lib.rs"),
        "only the edited file's targets should need embedding: {fresh:?}"
    );
    assert_eq!(changed["embedded_count"].as_u64(), Some(fresh.len() as u64));
    assert_eq!(
        changed["reused_embeddings"].as_u64(),
        Some((after.len() - fresh.len()) as u64)
    );

    // `rebuild` clears the build directory but not `current/embeddings.cache`, so this run
    // reuses every vector. Pinning the exact line asserts that reuse survives a rebuild,
    // rather than passing on any line that happens to start with the phase.
    let (stdout, stderr) = semantic("rebuild");
    let rebuilt = semantic_json_report(&stdout);
    assert_eq!(rebuilt["indexed_count"].as_u64(), Some(after.len() as u64));
    assert_eq!(rebuilt["embedded_count"].as_u64(), Some(0));
    assert_eq!(
        rebuilt["reused_embeddings"].as_u64(),
        Some(after.len() as u64)
    );
    assert!(!stderr.contains('\r'), "{stderr}");
    assert!(
        stderr
            .lines()
            .any(|line| line.starts_with("semantic[rebuild] nothing to embed")),
        "{stderr}"
    );
}

/// The rebuild arm's batch progress, in its own test so that neutralising it fails *here* rather
/// than being caught first by an assertion in the index test. A rebuild only does real work
/// once the embedding cache is gone, so the clean is the setup this test depends on.
#[test]
fn semantic_rebuild_reports_batch_progress_once_the_cache_is_cleared() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let (stdout, _) = run_semantic(&repo, "index");
    let built = semantic_json_report(&stdout);
    let indexed = built["indexed_count"].as_u64().unwrap();
    assert!(indexed > 0);

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("semantic")
            .arg("clean")
            .arg("--include-cache");
        command
    });
    // `clean` ignores removal errors, so prove the cache is gone before relying on it: a clean
    // that silently no-opped would leave the rebuild below warm and its assertions vacuous
    // again, and this assertion blames the clean rather than the rebuild.
    assert!(
        !semantic_current_dir(&repo)
            .join("embeddings.cache")
            .exists(),
        "clean --include-cache left the embedding cache behind"
    );

    let (stdout, stderr) = run_semantic(&repo, "rebuild");
    let cold = semantic_json_report(&stdout);
    let cold_indexed = cold["indexed_count"].as_u64().unwrap();
    assert!(cold_indexed > 0);
    assert_eq!(cold["reused_embeddings"].as_u64(), Some(0));
    assert_eq!(cold["embedded_count"].as_u64(), Some(cold_indexed));
    assert!(!stderr.contains('\r'), "{stderr}");
    // The completion count comes only from the per-batch callback, so a rebuild that reported
    // no batch progress would leave `0/N` here and fail.
    assert!(
        stderr.lines().any(|line| {
            line.starts_with("semantic[rebuild] ")
                && line.contains(&format!("{cold_indexed}/{cold_indexed} targets embedded"))
        }),
        "{stderr}"
    );
}

#[test]
fn demo_creates_indexed_sample_repo() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    let output = run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    assert!(output.contains("Open Kioku is ready"));
    assert!(repo.join("ok.toml").exists());
    assert!(repo.join(".ok/index.sqlite").exists());

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("issue_token");
        command
    });
    assert!(search.contains("src/auth.rs"));

    let graph_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--kind")
            .arg("graph")
            .arg("issue token")
            .arg("--limit")
            .arg("5");
        command
    });
    assert!(graph_search.contains("graph node identifier match"));
    assert!(graph_search.contains("graph_node_identifier"));
    assert!(graph_search.contains("issue_token"));

    let explained_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("search")
            .arg("issue_token")
            .arg("--explain-ranking");
        command
    });
    assert!(explained_search.contains("ranking:"));
    assert!(explained_search.contains("text_relevance"));

    // `--regex` must be exact matching, not the ranked lexical path wearing a
    // different flag: every hit reports the regex match reason at confidence 1.
    let regex_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--regex")
            .arg(r"fn\s+issue_token");
        command
    });
    assert!(regex_search.contains("src/auth.rs"));
    assert!(regex_search.contains("\"match_reason\": \"regex match\""));
    assert!(regex_search.contains("\"confidence\": 1.0"));
    assert!(regex_search.contains("\"caveats\""));

    // Exact matching and the ranked modes answer different questions; asking for
    // both is rejected rather than silently resolved by precedence.
    let (_, regex_conflict) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("search")
            .arg("--regex")
            .arg("--hybrid")
            .arg("issue_token");
        command
    });
    assert!(
        regex_conflict.contains("cannot be used with"),
        "--regex and --hybrid should conflict, got: {regex_conflict}"
    );

    let regex_no_match = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--regex")
            .arg("^struct NoSuchSymbolAnywhere");
        command
    });
    // A miss under --json must still say what corpus was searched; an empty
    // array alone would read as "absent from the repository".
    assert!(regex_no_match.contains("\"results\": []"));
    assert!(regex_no_match.contains("indexed chunk text"));
    assert!(regex_no_match.contains("regions the indexer did not chunk were not searched"));
    assert!(regex_no_match.contains("\"truncated\": false"));

    let regex_invalid = run_usage_error({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--regex")
            .arg("fn (");
        command
    });
    assert!(
        regex_invalid.contains("invalid input: regex parse error"),
        "an invalid pattern is a usage error, got: {regex_invalid}"
    );

    let regex_bounded = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--regex")
            .arg(".")
            .arg("--limit")
            .arg("2");
        command
    });
    assert_eq!(
        regex_bounded
            .matches("\"match_reason\": \"regex match\"")
            .count(),
        2
    );

    // `symbol context` promises the definition body, so it has to produce one —
    // and say plainly when part of the bundle is outside the indexed corpus.
    // `issue_token` is the first symbol in its file, so nothing above it was
    // chunked and the doc-comment caveat is the honest answer.
    let first_symbol_context = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("symbol")
            .arg("context")
            .arg("issue_token");
        command
    });
    assert!(first_symbol_context.contains("pub fn issue_token"));
    assert!(first_symbol_context.contains("format!(\\\"token:"));
    assert!(first_symbol_context.contains("\"body_range\""));
    assert!(first_symbol_context.contains("outside the indexed corpus"));

    // `validate_token` follows another definition, so the lines above it were
    // chunked and come back verbatim.
    let later_symbol_context = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("symbol")
            .arg("context")
            .arg("validate_token");
        command
    });
    assert!(later_symbol_context.contains("pub fn validate_token"));
    assert!(later_symbol_context.contains("pub fn issue_token"));
    assert!(!later_symbol_context.contains("outside the indexed corpus"));

    let semantic_status = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("semantic")
            .arg("status");
        command
    });
    assert!(semantic_status.contains("\"state\": \"disabled\""));

    let semantic_index = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("semantic")
            .arg("index");
        command
    });
    assert!(semantic_index.contains("\"state\": \"ready\""));
    assert!(semantic_index.contains("\"vector_count\""));
    assert!(repo.join(".ok/vectors/current/manifest.json").exists());

    let semantic_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--semantic")
            .arg("session token")
            .arg("--limit")
            .arg("5");
        command
    });
    assert!(semantic_json.contains("src/auth.rs"));
    assert!(semantic_json.contains("semantic_similarity"));

    let hybrid = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("search")
            .arg("--hybrid")
            .arg("--explain-ranking")
            .arg("session token");
        command
    });
    assert!(hybrid.contains("semantic_similarity"));

    let mcp_semantic_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 24,
        "method": "tools/call",
        "params": {
            "name": "search_code",
            "arguments": {"query": "session token", "mode": "hybrid", "limit": 5}
        }
    })
    .to_string();
    let mcp_semantic = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_semantic_req + "\n"),
    );
    assert!(mcp_semantic.contains("semantic_status"));
    assert!(mcp_semantic.contains("semantic_similarity"));

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(plan.contains("# Plan: token"));
    assert!(plan.contains("## Confidence"));
    assert!(plan.contains("## Negative Evidence"));
    assert!(plan.contains("## Evidence Provenance"));
    assert!(plan.contains("exact_references"));
    assert!(plan.contains("evidence:"));
    assert!(plan.contains("## Primary Context"));
    assert!(plan.contains("## Agent Tool Calls"));

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("plan")
            .arg("token");
        command
    });
    assert!(plan_json.contains("\"confidence_breakdown\""));
    assert!(plan_json.contains("\"overall_score\""));
    assert!(plan_json.contains("\"components\""));
    assert!(plan_json.contains("\"caveats\""));
    assert!(plan_json.contains("\"negative_evidence\""));
    assert!(plan_json.contains("\"evidence_by_section\""));
    assert!(plan_json.contains("\"evidence_refs\""));
    assert!(plan_json.contains("\"allowed_rules\""));
    assert!(plan_json.contains("\"caution_rules\""));
    assert!(plan_json.contains("\"forbidden_rules\""));
    assert!(plan_json.contains("\"expansion_requirements\""));
    let plan_value: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    assert_eq!(plan_value["evidence_quality"]["unresolved_import_count"], 0);
    assert_eq!(plan_value["evidence_quality"]["ambiguous_edge_count"], 0);
    assert!(plan_value["evidence_quality"]["failed_optional_passes"]
        .as_array()
        .unwrap()
        .is_empty());

    let plan_path = repo.join("plan.json");
    fs::write(&plan_path, &plan_json).unwrap();
    let verify_allowed = run({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    assert!(verify_allowed.contains("Boundary verification passed"));

    let (_boundary_stdout, boundary_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs");
        command
    });
    assert!(boundary_stderr.contains("out of saved plan boundary"));
    assert!(boundary_stderr.contains("boundary expansion requires explicit evidence"));

    let verify_expansion = run({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("search:src/out_of_scope.rs:1-2:0");
        command
    });
    assert!(verify_expansion.contains("Boundary verification passed"));

    let (_forbidden_stdout, forbidden_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("vendor/generated.rs")
            .arg("--evidence-ref")
            .arg("manual:vendor");
        command
    });
    assert!(forbidden_stderr.contains("forbidden boundary edit"));

    // Both invalid-input cases are matched per stderr line rather than by prefix: a build with
    // the `mem-profile` feature (CI coverage runs `--all-features`) reports allocation totals
    // on stderr before the error line.
    let stderr_has_line = |stderr: &str, prefix: &str| {
        stderr
            .lines()
            .any(|line| line.trim_start().starts_with(prefix))
    };
    // No change source at all is a usage error clap rejects before the plan is read.
    let no_source = ok()
        .arg("--repo")
        .arg(&repo)
        .arg("verify")
        .arg("--plan")
        .arg(&plan_path)
        .output()
        .unwrap();
    assert_eq!(no_source.status.code(), Some(2));
    let no_source_stderr = String::from_utf8_lossy(&no_source.stderr);
    assert!(
        stderr_has_line(
            &no_source_stderr,
            "error: the following required arguments were not provided"
        ),
        "{no_source_stderr}"
    );
    // A diff that names no file reaches the kernel check, which is caller input too.
    let empty_diff_path = repo.join("empty.diff");
    fs::write(&empty_diff_path, "").unwrap();
    let empty_diff = ok()
        .arg("--repo")
        .arg(&repo)
        .arg("verify")
        .arg("--plan")
        .arg(&plan_path)
        .arg("--diff")
        .arg(&empty_diff_path)
        .output()
        .unwrap();
    assert_eq!(empty_diff.status.code(), Some(2));
    let empty_diff_stderr = String::from_utf8_lossy(&empty_diff.stderr);
    assert!(
        stderr_has_line(
            &empty_diff_stderr,
            "Error: invalid input: verify requires at least one changed file or a non-empty unified diff"
        ),
        "{empty_diff_stderr}"
    );
    // The same assertion must hold with any diagnostic preamble on stderr, as the coverage
    // build produces; this pins the matcher without depending on how the binary was built.
    assert!(stderr_has_line(
        "ok[mem-profile] peak_live_bytes=1\nError: invalid input: verify requires at least one changed file or a non-empty unified diff\n",
        "Error: invalid input: verify requires at least one changed file or a non-empty unified diff"
    ));

    let verify_diff_path = repo.join("auth.diff");
    fs::write(
        &verify_diff_path,
        "diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -3,0 +4 @@\n+// verifier smoke\n",
    )
    .unwrap();
    let verify_pass = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--diff")
            .arg(&verify_diff_path);
        command
    });
    assert!(verify_pass.contains("\"verdict\": \"warn\""));
    assert!(verify_pass.contains("\"evidence_quality\""));
    assert!(verify_pass.contains("\"changed_symbols\""));

    // A comment-only hunk past every symbol of `src/auth.rs` has no symbol to name; the text
    // and HTML reports list it as JSON does rather than showing nothing for the change.
    let comment_diff_path = repo.join("auth-comment.diff");
    fs::write(
        &comment_diff_path,
        "diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -200,0 +201 @@\n+// trailing note\n",
    )
    .unwrap();
    let verify_region_text = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--diff")
            .arg(&comment_diff_path);
        command
    });
    assert!(
        verify_region_text.contains(
            "Changed regions without a symbol (no indexed symbol range covers these lines):\n  - src/auth.rs:201-201\n"
        ),
        "{verify_region_text}"
    );
    let verify_region_html = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--format")
            .arg("html")
            .arg("--diff")
            .arg(&comment_diff_path);
        command
    });
    assert!(
        verify_region_html.contains("<h2>Changed Regions Without a Symbol</h2>"),
        "{verify_region_html}"
    );
    assert!(
        verify_region_html.contains("<li><code>src/auth.rs:201-201</code></li>"),
        "{verify_region_html}"
    );

    let verify_warn = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("search:src/out_of_scope.rs:1-2:0");
        command
    });
    assert!(verify_warn.contains("\"verdict\": \"warn\""));
    assert!(verify_warn.contains("boundary_expansion"));
    assert!(verify_warn.contains("\"traceability\""));

    let (verify_strict_stdout, verify_strict_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("tampered:evidence")
            .arg("--traceability-strict");
        command
    });
    assert!(verify_strict_stdout.contains("\"verdict\": \"fail\""));
    assert!(verify_strict_stdout.contains("unknown_evidence_ref"));
    assert!(verify_strict_stderr.contains("change verification failed"));

    let (verify_fail_stdout, verify_fail_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs");
        command
    });
    assert!(verify_fail_stdout.contains("\"verdict\": \"fail\""));
    assert!(verify_fail_stdout.contains("out_of_boundary"));
    assert!(verify_fail_stderr.contains("change verification failed"));

    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "plan": plan_value,
                "changed_files": ["src/auth.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    assert!(mcp_verify.contains("structuredContent"));
    assert!(mcp_verify.contains("\"verdict\""));
    assert!(mcp_verify.contains("changed_symbols"));

    let (_warn_stdout, warn_stderr) = run_ok_with_stderr({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--verify-evidence")
            .arg("warn");
        command
    });
    assert!(warn_stderr.contains("negative evidence"));

    let (_fail_stdout, fail_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--verify-evidence")
            .arg("fail");
        command
    });
    assert!(fail_stderr.contains("plan evidence verification failed"));

    let eval = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("eval")
            .arg(&repo)
            .arg("--case")
            .arg("issue_token=src/auth.rs")
            .arg("--limit")
            .arg("5")
            .arg("--no-index");
        command
    });
    assert!(eval.contains("\"baseline\""));
    assert!(eval.contains("\"fusion\""));
    assert!(eval.contains("\"semantic\""));
    assert!(eval.contains("\"ablations\""));
    assert!(eval.contains("\"signal\": \"text_relevance\""));
    assert!(eval.contains("\"signal\": \"semantic_similarity\""));
    assert!(eval.contains("\"top_search_signals\""));

    let workflow_cases = repo.join("workflow-cases.json");
    fs::write(
        &workflow_cases,
        r#"[
          {
            "id": "auth-token",
            "task": "issue_token",
            "expected_primary_context": ["src/auth.rs"],
            "expected_boundary": ["src/auth.rs"],
            "changed_files": ["src/auth.rs"],
            "expected_verdict": "pass",
            "expected_confidence": true
          }
        ]"#,
    )
    .unwrap();
    let workflow_bench = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("workflow-bench")
            .arg(&repo)
            .arg("--cases-file")
            .arg(&workflow_cases)
            .arg("--limit")
            .arg("5")
            .arg("--min-cases")
            .arg("1")
            .arg("--no-index");
        command
    });
    assert!(workflow_bench.contains("\"workflow\""));
    assert!(workflow_bench.contains("\"deltas\""));
    assert!(workflow_bench.contains("\"context_recall_at_k\""));
    assert!(workflow_bench.contains("\"verification_verdict_accuracy\""));
}

#[test]
fn contract_cli_and_mcp_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    let _ = run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let create_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("create")
            .arg("token")
            .arg("--limit")
            .arg("5");
        command
    });
    let create: serde_json::Value = serde_json::from_str(&create_json).unwrap();
    let contract_id = create["contract_id"].as_str().unwrap().to_string();
    assert_eq!(create["stored"], true);
    assert!(repo
        .join(".ok/contracts")
        .join(format!("{contract_id}.json"))
        .exists());

    let show_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("show")
            .arg(&contract_id)
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(show_markdown.contains("# Change Contract"));

    let export_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("export")
            .arg(&contract_id)
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(export_toon.contains("type: change_contract"));

    let explain_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("explain")
            .arg("--id")
            .arg(&contract_id)
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(explain_markdown.contains("# Contract Explanation"));

    let verify_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("verify")
            .arg("--id")
            .arg(&contract_id)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    let verification: serde_json::Value = serde_json::from_str(&verify_json).unwrap();
    assert_eq!(verification["contract_id"], contract_id);
    assert!(verification["decision"].as_str().is_some());
    assert!(repo
        .join(".ok/contracts")
        .join(format!("{contract_id}.verify.jsonl"))
        .exists());

    let inline_contract = serde_json::to_string(&create["contract"]).unwrap();
    let inline_verify_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("verify")
            .arg("--contract-json")
            .arg(&inline_contract)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    let inline_verification: serde_json::Value = serde_json::from_str(&inline_verify_json).unwrap();
    assert_eq!(inline_verification["contract_id"], contract_id);

    let mcp_create = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"plan_change","arguments":{"task":"token","limit":5,"persist":true}}}"#,
    );
    let mcp_create: serde_json::Value = serde_json::from_str(mcp_create.trim()).unwrap();
    let mcp_contract_id = mcp_create["result"]["structuredContent"]["contract_id"]
        .as_str()
        .unwrap()
        .to_string();

    // `get_change_contract` moved to the CLI in 4.0.0; a stored contract is
    // fetched with `ok contract show`, not over MCP.
    let cli_get = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("show")
            .arg(&mcp_contract_id)
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(cli_get.contains("# Change Contract"));

    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "contract_id": mcp_contract_id,
                "changed_files": ["src/auth.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    let mcp_verify: serde_json::Value = serde_json::from_str(mcp_verify.trim()).unwrap();
    let mcp_report = mcp_verify["result"]["structuredContent"].clone();
    assert!(mcp_report["decision"].as_str().is_some());

    let mcp_explain_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "verification": mcp_report,
                "format": "markdown"
            }
        }
    })
    .to_string();
    let mcp_explain = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_explain_req + "\n"),
    );
    let mcp_explain: serde_json::Value = serde_json::from_str(mcp_explain.trim()).unwrap();
    assert!(mcp_explain["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("# Verification Explanation"));
    assert_eq!(
        mcp_explain["result"]["structuredContent"]["rendered_in"],
        "content"
    );
}

#[test]
fn memory_and_compressed_context_are_available() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let remembered = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("memory")
            .arg("remember")
            .arg("RATE-7031 maps issue_token to tests/auth_flow.rs")
            .arg("--source")
            .arg("cli-smoke")
            .arg("--confidence")
            .arg("high");
        command
    });
    assert!(remembered.contains("RATE-7031"));

    let memory = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("memory")
            .arg("search")
            .arg("RATE-7031 issue_token");
        command
    });
    assert!(memory.contains("entity link"));

    let compressed = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("context")
            .arg("token")
            .arg("--compressed");
        command
    });
    assert!(compressed.contains("\"handles\""));
    assert!(compressed.contains("ctx:"));

    let compressed_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("context")
            .arg("token")
            .arg("--compressed")
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(compressed_toon.contains("type: compressed_context_pack"));
    assert!(compressed_toon.contains("handles["));
    assert!(compressed_toon.contains("ctx:"));

    let parsed: serde_json::Value = serde_json::from_str(&compressed).unwrap();
    let handle = parsed["handles"][0]["id"].as_str().unwrap();
    let retrieved = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("retrieve-context")
            .arg(handle);
        command
    });
    assert!(retrieved.contains("token"));

    let plan_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(plan_toon.contains("type: plan_report"));
    assert!(plan_toon.contains("primary_context["));
}

#[test]
fn prove_generates_shareable_report_without_source_snippets() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let markdown = run({
        let mut command = ok();
        command
            .arg("prove")
            .arg(&repo)
            .arg("--task")
            .arg("token")
            .arg("--limit")
            .arg("8");
        command
    });
    assert!(markdown.contains("# Open Kioku Proof"));
    assert!(markdown.contains("Average proof score"));
    assert!(markdown.contains("Source snippets included: `false`"));
    assert!(!markdown.contains("pub fn issue_token"));

    let json = run({
        let mut command = ok();
        command
            .arg("prove")
            .arg(&repo)
            .arg("--task")
            .arg("token")
            .arg("--format")
            .arg("json");
        command
    });
    assert!(json.contains("\"generated_by\": \"ok prove\""));
    assert!(json.contains("\"source_snippets_included\": false"));
    assert!(json.contains("\"tasks_scored\": 1"));
}

#[test]
fn index_captures_git_history() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {:?}", args);
    };

    git(&["init", "--quiet"]);
    git(&["config", "user.email", "dev@example.com"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "commit.gpgsign", "false"]);

    fs::create_dir_all(repo.join(".github")).unwrap();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join(".github/CODEOWNERS"), "src/** dev@example.com\n").unwrap();
    fs::write(repo.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "first commit"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));

    fs::write(repo.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "second commit"]);

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("memory")
            .arg("remember")
            .arg("src/a.rs maintainer dev@example.com")
            .arg("--source")
            .arg("cli-smoke")
            .arg("--confidence")
            .arg("high");
        command
    });

    let index_output = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(index_output.contains("Indexed"));

    let store_path = repo.join(".ok/index.sqlite");
    let store = open_kioku_storage_sqlite::SqliteStore::open(store_path).unwrap();
    use open_kioku_storage::{HistoryStore, MetadataStore};
    let commits = store.recent_commits(10).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].summary, "second commit");
    assert_eq!(commits[1].summary, "first commit");

    let summary = store
        .history_for_file(std::path::Path::new("src/a.rs"), 10)
        .unwrap();
    assert_eq!(summary.recent_commits.len(), 1);
    assert_eq!(summary.recent_commits[0].summary, "first commit");

    let file_provenance = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let file_provenance: serde_json::Value = serde_json::from_str(&file_provenance).unwrap();
    assert_eq!(
        file_provenance["first_seen"]["commit"]["summary"],
        "first commit"
    );
    assert_eq!(
        file_provenance["last_touched"]["commit"]["summary"],
        "first commit"
    );

    let file_churn = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("churn")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let file_churn: serde_json::Value = serde_json::from_str(&file_churn).unwrap();
    assert_eq!(file_churn["stats"]["all_time"], 1);
    assert_eq!(file_churn["stats"]["last_90d"], 1);
    assert_eq!(file_churn["confidence"], "exact");

    let similar = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("similar")
            .arg("--task")
            .arg("first commit")
            .arg("--path")
            .arg("src/a.rs")
            .arg("--limit")
            .arg("5");
        command
    });
    let similar: serde_json::Value = serde_json::from_str(&similar).unwrap();
    assert_eq!(
        similar["hits"][0]["change"]["commit"]["summary"],
        "first commit"
    );
    let similar_sources = similar["hits"][0]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|evidence| evidence["source_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(similar_sources.contains(&"task_text"));
    assert!(similar_sources.contains(&"path"));

    let ownership = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("ownership")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let ownership: serde_json::Value = serde_json::from_str(&ownership).unwrap();
    assert_eq!(ownership["owners"][0]["owner"]["email"], "dev@example.com");
    let owner_sources = ownership["owners"][0]["source_types"].as_array().unwrap();
    assert!(owner_sources.iter().any(|source| source == "codeowners"));
    assert!(owner_sources.iter().any(|source| source == "git_history"));
    assert!(owner_sources.iter().any(|source| source == "repo_memory"));
    assert_eq!(ownership["owners"][0]["confidence"], "exact");

    let reviewers = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("reviewers")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let reviewers: serde_json::Value = serde_json::from_str(&reviewers).unwrap();
    assert_eq!(
        reviewers["availability"],
        "inferred_from_ownership_and_authors"
    );
    assert_eq!(
        reviewers["suggestions"][0]["reviewer"]["email"],
        "dev@example.com"
    );
    assert_eq!(reviewers["suggestions"][0]["actual_review_evidence"], false);
    assert_eq!(reviewers["suggestions"][0]["inferred_from_authors"], true);
    let reviewer_sources = reviewers["suggestions"][0]["source_types"]
        .as_array()
        .unwrap();
    assert!(reviewer_sources.iter().any(|source| source == "ownership"));
    assert!(reviewer_sources.iter().any(|source| source == "git_author"));
    assert!(reviewers["uncertainty"]
        .as_array()
        .unwrap()
        .iter()
        .any(|note| note
            .as_str()
            .unwrap()
            .contains("actual PR-review evidence is unavailable")));

    let symbol_provenance = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--symbol")
            .arg("a");
        command
    });
    let symbol_provenance: serde_json::Value = serde_json::from_str(&symbol_provenance).unwrap();
    assert_eq!(
        symbol_provenance["recent_touches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        symbol_provenance["recent_touches"][0]["commit"]["author"]["name"],
        "Test User"
    );
    assert!(!symbol_provenance["uncertainty"]
        .as_array()
        .unwrap()
        .is_empty());

    let symbol_id = store
        .list_symbols(Some("a"), 10, 0)
        .unwrap()
        .into_iter()
        .find(|symbol| symbol.name == "a")
        .unwrap()
        .id;
    let symbol_by_id = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--symbol")
            .arg(&symbol_id.0);
        command
    });
    let symbol_by_id: serde_json::Value = serde_json::from_str(&symbol_by_id).unwrap();
    assert_eq!(symbol_by_id["symbol_id"], symbol_id.0);

    // The history family moved to the CLI in 4.0.0. The reads below are the
    // shipped surface for the same evidence; MCP no longer answers them.
    let churn = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("churn")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let churn: serde_json::Value = serde_json::from_str(&churn).unwrap();
    assert_eq!(churn["stats"]["all_time"], 1);
    assert_eq!(churn["confidence"], "exact");

    let similar = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("similar")
            .arg("--task")
            .arg("first commit")
            .arg("--path")
            .arg("src/a.rs")
            .arg("--limit")
            .arg("5");
        command
    });
    let similar: serde_json::Value = serde_json::from_str(&similar).unwrap();
    assert_eq!(
        similar["hits"][0]["change"]["commit"]["summary"],
        "first commit"
    );

    let ownership = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("ownership")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let ownership: serde_json::Value = serde_json::from_str(&ownership).unwrap();
    assert_eq!(ownership["owners"][0]["owner"]["email"], "dev@example.com");
    assert!(ownership["owners"][0]["source_types"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source == "repo_memory"));

    let reviewers = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("reviewers")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let reviewers: serde_json::Value = serde_json::from_str(&reviewers).unwrap();
    assert_eq!(
        reviewers["availability"],
        "inferred_from_ownership_and_authors"
    );
    assert_eq!(
        reviewers["suggestions"][0]["reviewer"]["email"],
        "dev@example.com"
    );
    assert_eq!(reviewers["suggestions"][0]["actual_review_evidence"], false);
    assert_eq!(reviewers["suggestions"][0]["inferred_from_authors"], true);

    // A retired name must fail loudly rather than resolve to something else.
    let retired = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"history_provenance_lookup","arguments":{"path":"src/a.rs","limit":5}}}"#,
    );
    let retired: serde_json::Value = serde_json::from_str(retired.trim()).unwrap();
    assert!(retired["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("history_provenance_lookup"));
}

#[test]
fn reviewer_benchmark_corpus_passes() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("history")
            .arg("reviewers-bench")
            .arg("--min-accuracy")
            .arg("0.80");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["case_count"], 5);
    assert!(
        report["accuracy"].as_f64().unwrap() >= 0.80,
        "reviewer benchmark report: {report}"
    );
    assert!(report["failures"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_reports_a_source_tree_excluded_by_policy_with_its_governing_setting() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    fs::write(repo.join(".gitignore"), "src/\n").unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    let doctor = run({
        let mut command = ok();
        command.arg("--json").arg("doctor").arg(repo);
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "coverage")
        .expect("doctor has a coverage check");
    assert_eq!(check["status"], "warn");
    let message = check["message"].as_str().unwrap();
    // ok.toml is the one recognised file left; the source tree is named as excluded,
    // with the rule that excluded it, rather than reported as never discovered.
    assert!(
        message.starts_with("no programming-language files considered under the current policy"),
        "{message}"
    );
    assert!(
        message.contains("1 excluded by policy (1 ignored; 1 under src/; `.gitignore` governs the largest share)"),
        "{message}"
    );
    let step = doctor["next_steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step.as_str())
        .find(|step| step.starts_with("Coverage:"))
        .expect("an emptied ratio carries a next step");
    assert!(step.contains("`.gitignore` governs that"), "{step}");
}

/// `src/` holds 25 Rust files, `FrobnicateRegistry` among them, beside two files under `app/`.
/// With `git_ignore_src`, `.gitignore` lists `src/`. Initialised and indexed.
fn coverage_gap_fixture(git_ignore_src: bool) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("app")).unwrap();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("app/main.rs"),
        "pub fn live_entry() -> u32 {\n    1\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("app/session.rs"),
        "pub fn refresh_session() -> u32 {\n    2\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/registry.rs"),
        "pub struct FrobnicateRegistry;\n",
    )
    .unwrap();
    for index in 0..24 {
        fs::write(
            repo.join(format!("src/widget_{index}.rs")),
            format!("pub fn widget_{index}() {{}}\n"),
        )
        .unwrap();
    }
    if git_ignore_src {
        fs::write(repo.join(".gitignore"), "src/\n").unwrap();
    }
    for step in ["init", "index"] {
        run({
            let mut command = ok();
            command.arg(step).arg(repo);
            command
        });
    }
    temp
}

const COVERAGE_GAP_TASK: &str = "fix FrobnicateRegistry lookup in live_entry";

/// `[context, plan, status]` for `task` as JSON from the CLI, then the same three from MCP
/// `build_context_pack`, `plan_change` and `repo_status`.
fn coverage_surfaces(
    repo: &std::path::Path,
    task: &str,
) -> ([serde_json::Value; 3], [serde_json::Value; 3]) {
    let parse = |output: String| -> serde_json::Value {
        serde_json::from_str(&output).expect("command prints JSON")
    };
    let cli_context = parse(run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("context")
            .arg(task)
            .arg("--format")
            .arg("json");
        command
    }));
    let cli_plan = parse(run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("plan")
            .arg(task)
            .arg("--format")
            .arg("json");
        command
    }));
    let cli_status = parse(run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    }));

    let requests = [
        (
            "build_context_pack",
            serde_json::json!({"task": task, "format": "json"}),
        ),
        (
            "plan_change",
            serde_json::json!({"task": task, "format": "json"}),
        ),
        ("repo_status", serde_json::json!({})),
    ];
    let stdin = requests
        .iter()
        .enumerate()
        .map(|(id, (name, arguments))| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments}
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let output = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &stdin,
    );
    let mut responses = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON-RPC line"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), requests.len(), "{output}");
    responses.sort_by_key(|response| response["id"].as_u64());
    let mcp = |index: usize| {
        let result = &responses[index]["result"];
        assert!(result.is_object(), "{}", responses[index]);
        result["structuredContent"].clone()
    };
    (
        [cli_context, cli_plan, cli_status],
        [mcp(0), mcp(1), mcp(2)],
    )
}

fn coverage_negative_evidence(report: &serde_json::Value) -> Option<&serde_json::Value> {
    report["negative_evidence"]
        .as_array()
        .expect("negative_evidence list")
        .iter()
        .find(|item| item["scope"] == "coverage")
}

fn coverage_caveats(report: &serde_json::Value) -> Vec<String> {
    report["confidence_breakdown"]["caveats"]
        .as_array()
        .expect("caveats list")
        .iter()
        .filter_map(|caveat| caveat.as_str())
        // Gap caveats only. `UNRECORDED_COVERAGE_CAVEAT` also begins "index coverage", and on
        // a fixture without a coverage record the bare prefix would count the wrong sentence.
        .filter(|caveat| {
            *caveat != open_kioku_core::UNRECORDED_COVERAGE_CAVEAT
                && *caveat != open_kioku_core::UNREADABLE_COVERAGE_CAVEAT
                && caveat.starts_with("index coverage: ")
        })
        .map(str::to_owned)
        .collect()
}

/// A task naming a symbol defined in git-ignored source gets the coverage caveat, the
/// `coverage` negative evidence, the `index_coverage` component and a Low label from context and
/// plan on both surfaces, never a probe saying the name does not exist, and `repo_status` reports
/// the same verdict `ok --json status` does.
#[test]
fn git_ignored_source_reaches_context_plan_and_status_on_cli_and_mcp() {
    let temp = coverage_gap_fixture(true);
    let (cli, mcp) = coverage_surfaces(temp.path(), COVERAGE_GAP_TASK);
    for (surface, reports) in [("cli", &cli), ("mcp", &mcp)] {
        for (kind, report) in [("context", &reports[0]), ("plan", &reports[1])] {
            let label = format!("{surface} {kind}");
            let item = coverage_negative_evidence(report)
                .unwrap_or_else(|| panic!("{label}: no coverage negative evidence: {report}"));
            assert!(
                item["reason"]
                    .as_str()
                    .unwrap()
                    .contains("25 of 27 rust source files (92.6%) are not indexed (git-ignore)"),
                "{label}: {item}"
            );
            assert!(
                item["inspected_sources"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|source| source == "coverage:rust:git_ignore"),
                "{label}: {item}"
            );
            assert!(
                report["negative_evidence"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|item| item["suggested_next_probe"].as_str())
                    .all(|probe| !probe.contains("does not exist")),
                "{label}: {report}"
            );
            let confidence = &report["confidence_breakdown"];
            assert_eq!(confidence["overall_enum"], "low", "{label}: {confidence}");
            assert!(
                confidence["overall_score"].as_f64().unwrap() <= 0.50,
                "{label}: {confidence}"
            );
            assert_eq!(coverage_caveats(report).len(), 1, "{label}: {confidence}");
            assert!(
                confidence["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|component| component["signal"] == "index_coverage"
                        && component["evidence_ids"]
                            == serde_json::json!(["coverage:rust:git_ignore"])),
                "{label}: {confidence}"
            );
        }
    }
    for index in 0..2 {
        assert_eq!(coverage_caveats(&cli[index]), coverage_caveats(&mcp[index]));
        // The same item on both surfaces, compared field by field rather than whole. `ok
        // context` renders the pack to JSON text, where the f32 `confidence` prints as `0.9`;
        // MCP builds the value in memory, where the same f32 widens to `0.8999999761581421`.
        // The stored number is identical, so only its JSON spelling differs.
        let cli_item = coverage_negative_evidence(&cli[index]).expect("cli coverage item");
        let mcp_item = coverage_negative_evidence(&mcp[index]).expect("mcp coverage item");
        for field in [
            "scope",
            "query",
            "reason",
            "inspected_sources",
            "suggested_next_probe",
        ] {
            assert_eq!(cli_item[field], mcp_item[field], "{field}");
        }
        let cli_confidence = cli_item["confidence"].as_f64().expect("cli confidence");
        let mcp_confidence = mcp_item["confidence"].as_f64().expect("mcp confidence");
        assert!(
            (cli_confidence - mcp_confidence).abs() < 1e-6,
            "{cli_confidence} vs {mcp_confidence}"
        );
    }

    let gaps = &cli[2]["coverage_gaps"];
    assert_eq!(gaps, &mcp[2]["coverage_gaps"]);
    assert_eq!(gaps.as_array().map(Vec::len), Some(1), "{gaps}");
    assert_eq!(gaps[0]["language"], "rust");
    assert_eq!(gaps[0]["cause"], "git_ignore");
    assert_eq!(gaps[0]["missing_files"], 25);
    assert_eq!(gaps[0]["language_files"], 27);
}

/// The same repository without the ignore rule: no coverage signal anywhere, and an empty
/// verdict on both status surfaces.
#[test]
fn complete_coverage_adds_no_coverage_signal_on_cli_or_mcp() {
    let temp = coverage_gap_fixture(false);
    let (cli, mcp) = coverage_surfaces(temp.path(), COVERAGE_GAP_TASK);
    for (surface, reports) in [("cli", &cli), ("mcp", &mcp)] {
        for (kind, report) in [("context", &reports[0]), ("plan", &reports[1])] {
            let label = format!("{surface} {kind}");
            assert!(
                coverage_negative_evidence(report).is_none(),
                "{label}: {report}"
            );
            assert!(coverage_caveats(report).is_empty(), "{label}: {report}");
            let confidence = &report["confidence_breakdown"];
            assert!(
                confidence["components"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|component| component["signal"] != "index_coverage"),
                "{label}: {confidence}"
            );
            assert!(
                confidence["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|blocker| blocker.as_str())
                    .all(|blocker| !blocker.contains("index excluded")),
                "{label}: {confidence}"
            );
        }
        assert_eq!(
            reports[2]["coverage_gaps"],
            serde_json::json!([]),
            "{surface}: {}",
            reports[2]
        );
    }
}

/// A Python repository whose answer lives in `src/`. With `with_venv`, a git-ignored `venv/`
/// holds 30 site-packages modules, more than `src/` holds. Discovery descends into git-ignored
/// directories and records each file as `git_ignore`, so that is a coverage gap; only
/// name-pruned directories (`.venv`, `build`, `dist`, `node_modules`, `target`) count as one
/// pruned directory instead.
fn python_venv_fixture(with_venv: bool) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join(".gitignore"), "venv/\n").unwrap();
    fs::write(
        repo.join("src/session.py"),
        "def refresh_session_token(session):\n    session.expires_at = session.issued_at + 3600\n    return session\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/accounts.py"),
        "class AccountStore:\n    def load(self, account_id):\n        return account_id\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/billing.py"),
        "def charge_invoice(invoice):\n    return invoice\n",
    )
    .unwrap();
    if with_venv {
        for index in 0..30 {
            let package = repo.join(format!("venv/lib/python3.12/site-packages/package_{index}"));
            fs::create_dir_all(&package).unwrap();
            fs::write(
                package.join("__init__.py"),
                format!("def helper_{index}():\n    return {index}\n"),
            )
            .unwrap();
        }
    }
    for step in ["init", "index"] {
        run({
            let mut command = ok();
            command.arg(step).arg(repo);
            command
        });
    }
    temp
}

/// Rust source beside a git-ignored `venv/` that holds the repository's only Python. The gap
/// is Python; the selection can only be Rust, which is the control for "a gap in a language
/// the selection does not use". It needs its own repository: on a handful of files the pack
/// selects nearly everything, so a Rust file sitting beside Python source would not isolate it.
fn rust_source_venv_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join(".gitignore"), "venv/\n").unwrap();
    fs::write(
        repo.join("src/rotation.rs"),
        "pub fn rotate_widget_key(previous: u32) -> u32 {\n    previous + 1\n}\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub mod rotation;\n\npub fn widget_entry() -> u32 {\n    rotation::rotate_widget_key(1)\n}\n",
    )
    .unwrap();
    for index in 0..30 {
        let package = repo.join(format!("venv/lib/python3.12/site-packages/package_{index}"));
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("__init__.py"),
            format!("def helper_{index}():\n    return {index}\n"),
        )
        .unwrap();
    }
    for step in ["init", "index"] {
        run({
            let mut command = ok();
            command.arg(step).arg(repo);
            command
        });
    }
    temp
}

/// A git-ignored `venv/` larger than `src/` is a majority Python gap. A Python answer is
/// capped below `High` even though the task's identifier resolves in `src/`: the label follows
/// what the index holds, not how the task was phrased. A Rust answer over the same index is
/// untouched, and the same repository without `venv/` reports no gap at all - the two controls
/// that make this fail if the cap were dropped or applied everywhere.
#[test]
fn a_git_ignored_venv_caps_a_python_answer_and_leaves_a_rust_answer_alone() {
    let python_task = "fix refresh_session_token expiry handling";
    let rust_task = "fix rotate_widget_key rotation";
    let with_venv = python_venv_fixture(true);
    let without_venv = python_venv_fixture(false);

    let language_blocker = |report: &serde_json::Value| -> Vec<String> {
        report["confidence_breakdown"]["blockers"]
            .as_array()
            .expect("blockers")
            .iter()
            .filter_map(|blocker| blocker.as_str())
            .filter(|blocker| blocker.starts_with("the selected context is in a language"))
            .map(str::to_owned)
            .collect()
    };
    let preflight_verdict = |repo: &std::path::Path, task: &str| -> String {
        let out = run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .arg("preflight")
                .arg(task)
                .arg("--format")
                .arg("json");
            command
        });
        let value: serde_json::Value = serde_json::from_str(&out).expect("preflight json");
        value["verdict"].as_str().expect("verdict").to_owned()
    };

    // The Python answer: gap reported, selection in the gap's language, capped below High.
    let (cli, mcp) = coverage_surfaces(with_venv.path(), python_task);
    let gaps = &cli[2]["coverage_gaps"];
    assert_eq!(gaps.as_array().map(Vec::len), Some(1), "{gaps}");
    assert_eq!(gaps[0]["language"], "python");
    assert_eq!(gaps[0]["missing_files"], 30);
    for (surface, reports) in [("cli", &cli), ("mcp", &mcp)] {
        for (index, kind) in [(0, "context"), (1, "plan")] {
            let label = format!("{surface} {kind}");
            let report = &reports[index];
            assert!(
                coverage_negative_evidence(report).is_some(),
                "{label}: {report}"
            );
            assert_eq!(
                language_blocker(report).len(),
                1,
                "{label}: expected the excluded-language blocker: {report}"
            );
            let confidence = &report["confidence_breakdown"];
            // The cap is an f32 `0.74`, which widens to 0.7400000095367432 in JSON, so the
            // comparison carries an f32-epsilon tolerance rather than testing the encoder.
            assert!(
                confidence["overall_score"].as_f64().unwrap() <= 0.74 + 1e-6,
                "{label}: {confidence}"
            );
            assert!(
                confidence["overall_enum"] != "exact" && confidence["overall_enum"] != "high",
                "{label}: {confidence}"
            );
        }
    }
    // The field an agent branches on, not only the label it displays: a majority gap in the
    // language being edited withholds `safe_to_start`, like an unresolved import does.
    assert_eq!(
        preflight_verdict(with_venv.path(), python_task),
        "start_with_caution"
    );
    // The control: the same task over the index without the gap still starts safely, so a
    // trigger that fired for any reason at all would turn this red.
    assert_eq!(
        preflight_verdict(without_venv.path(), python_task),
        "safe_to_start"
    );

    // Control one: Rust source whose only Python sits in the git-ignored tree. The gap is
    // still reported - it is a fact about the index, not about the task - but the cap must not
    // apply, because the excluded files cannot hold a Rust definition.
    let rust_repo = rust_source_venv_fixture();
    let (rust_cli, _) = coverage_surfaces(rust_repo.path(), rust_task);
    assert_eq!(rust_cli[2]["coverage_gaps"][0]["language"], "python");
    for (index, kind) in [(0, "context"), (1, "plan")] {
        let report = &rust_cli[index];
        assert!(
            coverage_negative_evidence(report).is_some(),
            "rust {kind}: the gap is a fact about the index, reported for any task: {report}"
        );
        assert!(
            language_blocker(report).is_empty(),
            "rust {kind}: a python gap must not cap a rust selection: {report}"
        );
    }

    // Control two: the same repository without `venv/` reports no gap at all.
    let (base_cli, base_mcp) = coverage_surfaces(without_venv.path(), python_task);
    assert_eq!(base_cli[2]["coverage_gaps"], serde_json::json!([]));
    assert_eq!(base_mcp[2]["coverage_gaps"], serde_json::json!([]));
    for (index, kind) in [(0, "context"), (1, "plan")] {
        let report = &base_cli[index];
        assert!(
            coverage_negative_evidence(report).is_none(),
            "{kind}: {report}"
        );
        assert!(language_blocker(report).is_empty(), "{kind}: {report}");
    }
}

/// The human surfaces print the gap beside the coverage summary. Without this, a refactor
/// collapsing the coverage print back into a single `summary_line()` call passes fmt, clippy,
/// every test and every snapshot family, and the defect returns silently: the summary ratio is
/// computed over considered files, so it reads near-100% on exactly the repository a gap
/// describes. All three surfaces print the same sentence.
#[test]
fn coverage_gaps_are_printed_by_index_status_and_markdown() {
    let gapped = coverage_gap_fixture(true);
    let sentence =
        "index coverage: 25 of 27 rust source files (92.6%) are not indexed (git-ignore)";

    let indexed = run({
        let mut command = ok();
        command.arg("index").arg(gapped.path());
        command
    });
    assert!(
        indexed.contains(&format!("coverage gap: {sentence}")),
        "ok index must print the gap beside the summary: {indexed}"
    );

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(gapped.path()).arg("status");
        command
    });
    assert!(
        status.contains(&format!("Coverage gap: {sentence}")),
        "ok status must print the gap: {status}"
    );

    let markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(gapped.path())
            .arg("status")
            .arg("--markdown");
        command
    });
    assert!(
        markdown.contains("| Coverage gaps |") && markdown.contains(sentence),
        "ok status --markdown must carry the gap row: {markdown}"
    );

    // Control: a fully covered repository prints none of it, so an unconditional print would
    // turn this red rather than pass unnoticed.
    let covered = coverage_gap_fixture(false);
    let indexed = run({
        let mut command = ok();
        command.arg("index").arg(covered.path());
        command
    });
    assert!(!indexed.contains("coverage gap:"), "{indexed}");
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(covered.path()).arg("status");
        command
    });
    assert!(!status.contains("Coverage gap:"), "{status}");
    let markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(covered.path())
            .arg("status")
            .arg("--markdown");
        command
    });
    assert!(!markdown.contains("| Coverage gaps |"), "{markdown}");
}

#[test]
fn index_reports_coverage_in_summary_status_and_doctor() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("vendor")).unwrap();
    fs::create_dir_all(repo.join(".aws")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    // Excluded by the vendor detector and the secret-path rule respectively: policy
    // exclusions, reported beside the ratio. The binary file is the omission the ratio
    // is judged on.
    fs::write(repo.join("vendor/dep.rs"), "pub fn vendored() {}\n").unwrap();
    fs::write(repo.join(".aws/credentials.json"), "{}\n").unwrap();
    fs::write(repo.join("src/blob.rs"), b"pub fn blob() {}\0").unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    let indexed = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    let coverage_line = indexed
        .lines()
        .find(|line| line.starts_with("coverage: "))
        .expect("index prints a coverage line");
    // The judged ratio counts source the policy would consider (1 of 2 Rust files; the
    // vendored one is excluded); the all-languages ratio is reported beside it.
    assert!(
        coverage_line.contains("1 of 2 programming-language files indexed (50.0%)"),
        "{coverage_line}"
    );
    assert!(
        coverage_line.contains("2 of 3 recognised files indexed (66.7%) overall; 2 excluded by policy (1 vendor, 1 secret-policy; 1 under vendor/"),
        "{coverage_line}"
    );
    assert!(
        coverage_line.contains("skipped: 1 binary"),
        "{coverage_line}"
    );

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    let rust = &status["coverage"]["by_language"]["rust"];
    assert_eq!(rust["discovered"], 3);
    assert_eq!(rust["indexed"], 1);
    assert_eq!(rust["skipped"]["vendor"], 1);
    assert_eq!(rust["skipped"]["binary"], 1);
    assert_eq!(
        status["coverage"]["by_language"]["json"]["skipped"]["secret_policy"],
        1
    );
    assert_eq!(status["coverage"]["policy_excluded_dirs"]["vendor"], 1);
    assert_eq!(
        status["coverage"]["policy_excluded_by_source"]["security_policy"],
        1
    );
    assert_eq!(
        status["coverage"]["policy_excluded_by_language"]["rust"]["detector"],
        1
    );
    assert_eq!(
        status["coverage"]["policy_excluded_by_language"]["json"]["security_policy"],
        1
    );
    assert_eq!(status["quality"]["coverage"]["by_language"]["rust"], *rust);

    let doctor = run({
        let mut command = ok();
        command.arg("--json").arg("doctor").arg(repo);
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    assert_eq!(doctor["coverage"]["by_language"]["rust"], *rust);
    assert_eq!(
        doctor["coverage"]["policy_excluded_by_language"],
        status["coverage"]["policy_excluded_by_language"]
    );
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "coverage")
        .expect("doctor has a coverage check");
    assert_eq!(check["status"], "warn");
    let message = check["message"].as_str().unwrap();
    assert!(message.contains("top skip reasons: 1 binary"), "{message}");
    assert!(message.contains("2 excluded by policy"), "{message}");
    // Two Rust files are under the per-language floor, so no language is named; the
    // programming-language ratio itself (1 of 2) is what warns.
    assert!(
        message.contains("1 of 2 programming-language files indexed (50.0%)"),
        "{message}"
    );
    assert!(!message.contains("under 98%:"), "{message}");
    // The advice names the omission the ratio was judged on, not an unrelated key.
    let step = doctor["next_steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step.as_str())
        .find(|step| step.starts_with("Coverage:"))
        .expect("coverage warning carries a next step");
    assert!(step.contains("skipped as binary"), "{step}");
    assert!(!step.contains("[index] exclude"), "{step}");

    let doctor_text = run({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(doctor_text.contains("Coverage by language:"));
    assert!(doctor_text
        .lines()
        .any(|line| line.trim_start().starts_with("rust ") && line.contains("50.0%!")));
    assert!(
        doctor_text.contains("Excluded by policy: 2 (1 vendor, 1 secret-policy)"),
        "{doctor_text}"
    );
    assert!(
        doctor_text.contains("governing setting: `[paths] deny` or the built-in secret-path rule"),
        "{doctor_text}"
    );
}

#[test]
fn context_pack_widens_top_file_regions_and_costs_supporting_files_in_the_ledger() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("context")
            .arg("issue token");
        command
    });
    let pack: serde_json::Value = serde_json::from_str(&output).unwrap();
    let primary = pack["primary_files"].as_array().unwrap();
    let supporting = pack["supporting_files"].as_array().unwrap();
    let selection = &pack["retrieval_diagnostics"]["selection"];
    let units = selection["selected_units"].as_array().unwrap();

    // The ledger lists every primary unit first, then every supporting file, and its total is
    // the sum of what it lists.
    assert_eq!(units.len(), primary.len() + supporting.len());
    let (primary_units, supporting_units) = units.split_at(primary.len());
    for (unit, result) in primary_units.iter().zip(primary) {
        assert_eq!(unit["path"], result["path"]);
        assert_eq!(unit["line_range"], result["line_range"]);
    }
    for (unit, result) in supporting_units.iter().zip(supporting) {
        assert_eq!(unit["path"], result["path"]);
        assert!(unit["rationale"]
            .as_str()
            .unwrap()
            .contains("not selected under the context budget"));
    }
    let total = units
        .iter()
        .map(|unit| unit["estimated_tokens"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(
        selection["estimated_tokens_selected"].as_u64().unwrap(),
        total
    );

    // The top file's region was widened, every step is an evidence ref named in the
    // rationale, and the widened range is what the primary snippet actually shows.
    let first = &primary_units[0];
    let region_refs = first["evidence_refs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|reference| reference.as_str().unwrap().starts_with("region:"))
        .count();
    assert!(region_refs > 0, "{first}");
    assert!(first["rationale"]
        .as_str()
        .unwrap()
        .contains("region widened: "));
    let range = &primary[0]["line_range"];
    let shown = primary[0]["snippet"].as_str().unwrap().split('\n').count() as u64;
    assert_eq!(
        range["end"].as_u64().unwrap() - range["start"].as_u64().unwrap() + 1,
        shown
    );
    assert_eq!(
        selection["unattributed_selected_file_count"].as_u64(),
        Some(0)
    );
    assert_eq!(selection["budget"]["region_files"].as_u64(), Some(3));
}

/// A repository nobody has indexed is told so in one sentence by every read surface, and
/// none of them creates `.ok` on the way: an empty database left behind by a read used to
/// make every later read report a legacy index awaiting rebuild instead.
#[test]
fn unindexed_repository_reads_say_not_indexed_and_create_nothing() {
    let temp = tempfile::tempdir().unwrap();
    // `ok doctor <repo>` canonicalizes the positional path; `--repo` is used as given. The
    // sentence names the path, so hand every command the canonical one.
    let repo = temp.path().canonicalize().unwrap();
    let repo = repo.as_path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )
    .unwrap();
    let repo_arg = repo.display().to_string();
    let next_step = format!("ok index {repo_arg}");

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("status");
        command
    });
    assert!(
        status.contains("repository is not indexed") && status.contains(&next_step),
        "{status}"
    );

    let status_json = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status_json: serde_json::Value = serde_json::from_str(&status_json).unwrap();
    assert_eq!(status_json["indexed"], false, "{status_json}");
    assert_eq!(status_json["next_step"], next_step, "{status_json}");
    assert!(status_json["message"]
        .as_str()
        .unwrap()
        .starts_with("repository is not indexed"));

    for args in [
        vec!["search", "Worker"],
        vec!["context", "change Worker::run"],
        vec!["impact", "--file", "src/lib.rs"],
        vec!["plan", "change Worker::run"],
    ] {
        let (_stdout, stderr) = run_failure({
            let mut command = ok();
            command.arg("--repo").arg(repo).args(&args);
            command
        });
        assert!(
            stderr.contains("repository is not indexed") && stderr.contains(&next_step),
            "{args:?} must say the repository is not indexed, got: {stderr}"
        );
        assert!(
            !stderr.contains("legacy index"),
            "{args:?} must not describe a never-indexed repository as a legacy index: {stderr}"
        );
    }

    // Neither command needs the index, so neither may create the sidecar stores under
    // `.ok` while answering from nothing.
    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .args(["retrieve-context", "bogus"]);
        command
    });
    assert!(stderr.contains("no context handle `bogus`"), "{stderr}");
    for args in [
        vec!["memory", "recent"],
        vec!["memory", "search", "anything"],
    ] {
        let stdout = run({
            let mut command = ok();
            command.arg("--repo").arg(repo).arg("--json").args(&args);
            command
        });
        assert_eq!(stdout.trim(), "[]", "{args:?}: {stdout}");
    }
    assert!(
        !repo.join(".ok").exists(),
        "retrieve-context and memory reads must not create .ok"
    );

    // The doctor's MCP probe spawns a real server; it must not index either.
    let (doctor, _stderr) = run_failure({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(
        doctor.contains("[fail] index") && doctor.contains("repository is not indexed"),
        "{doctor}"
    );
    assert!(doctor.contains(&next_step), "{doctor}");

    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command
                .arg("mcp")
                .arg("serve")
                .arg("--repo")
                .arg(repo)
                .arg("--read-only");
            command
        },
        concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"repo_status","arguments":{}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"Worker"}}}"#,
            "\n",
        ),
    );
    let responses = mcp
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 4, "{mcp}");
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "open-kioku");
    assert!(responses[1]["result"]["tools"].is_array());
    let repo_status = &responses[2]["result"];
    assert_eq!(repo_status["isError"], false);
    assert_eq!(repo_status["structuredContent"]["indexed"], false);
    assert_eq!(repo_status["structuredContent"]["next_step"], next_step);
    let search_error = responses[3]["error"]["message"].as_str().unwrap();
    assert!(
        search_error.contains("repository is not indexed"),
        "{search_error}"
    );
    assert!(search_error.contains(&next_step), "{search_error}");
    // Same sentence on both surfaces.
    assert_eq!(search_error, status_json["message"].as_str().unwrap());

    assert!(
        !repo.join(".ok").exists(),
        "no read surface may create .ok in an unindexed repository"
    );
}

/// A `.mcp.json` entry the user wrote by hand (what `ok mcp install claude` prints) is
/// adopted when it launches this repository's server, before anything is indexed, and the
/// tracked file is not rewritten; an entry pointing elsewhere is refused before indexing.
#[test]
fn agent_setup_adopts_a_hand_written_entry_and_refuses_an_incompatible_one_before_indexing() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"onboarding-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn answer() -> u8 { 42 }\n").unwrap();
    let hand_written = "{\n  \"mcpServers\": {\n    \"open-kioku\": {\n      \"command\": \"ok\",\n      \"args\": [\"mcp\", \"serve\", \"--repo\", \".\"],\n      \"env\": {}\n    }\n  }\n}\n";
    fs::write(repo.join(".mcp.json"), hand_written).unwrap();

    let applied = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("claude")
            .arg("--repo")
            .arg(repo)
            .arg("--apply");
        command
    });
    assert!(applied.contains("[kept] config"), "{applied}");
    assert!(applied.contains("existing entry preserved"), "{applied}");
    assert!(applied.contains("[applied] skill"), "{applied}");
    assert!(applied.contains("[passed] mcp_stdio"), "{applied}");
    assert_eq!(
        fs::read_to_string(repo.join(".mcp.json")).unwrap(),
        hand_written,
        "an adopted entry is not rewritten"
    );
    assert!(repo.join(".claude/skills/open-kioku/SKILL.md").is_file());
    assert!(repo.join(".ok/index.sqlite").is_file());

    let checked = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("claude")
            .arg("--repo")
            .arg(repo)
            .arg("--check");
        command
    });
    assert!(checked.contains("[passed] config"), "{checked}");
    assert!(
        checked.contains("Open Kioku is ready for this repository."),
        "{checked}"
    );

    let other = tempfile::tempdir().unwrap();
    let other_repo = other.path();
    fs::create_dir_all(other_repo.join("src")).unwrap();
    fs::write(
        other_repo.join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    fs::write(
        other_repo.join(".mcp.json"),
        r#"{"mcpServers":{"open-kioku":{"command":"ok","args":["mcp","serve","--repo","/somewhere/else"]}}}"#,
    )
    .unwrap();
    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("claude")
            .arg("--repo")
            .arg(other_repo)
            .arg("--apply");
        command
    });
    assert!(stderr.contains("`mcpServers.open-kioku`"), "{stderr}");
    assert!(stderr.contains("serves `/somewhere/else`"), "{stderr}");
    assert!(stderr.contains("\"--read-only\""), "{stderr}");
    assert!(
        !other_repo.join(".ok").exists(),
        "an incompatible entry is refused before indexing"
    );
    assert!(!other_repo.join(".claude").exists());

    let (check_stdout, check_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("setup")
            .arg("agent")
            .arg("claude")
            .arg("--repo")
            .arg(other_repo)
            .arg("--check");
        command
    });
    assert!(check_stdout.contains("[mismatch] config"), "{check_stdout}");
    assert!(check_stdout.contains("/somewhere/else"), "{check_stdout}");
    assert!(
        !check_stdout.contains("--apply") && !check_stderr.contains("--apply"),
        "--check must not recommend a command that fails on this state:\n{check_stdout}\n{check_stderr}"
    );
}

fn init_and_index_worker_repo() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    // `ok doctor <repo>` canonicalizes the positional path; the messages name the path, so
    // hand every command the canonical one.
    let repo = temp.path().canonicalize().unwrap();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(&repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });
    (temp, repo)
}

/// The `repo_status` response of a fresh MCP session, after checking that the session
/// answered `tools/list` afterwards, whatever the probe found.
fn mcp_repo_status(repo: &std::path::Path) -> serde_json::Value {
    let output = run_with_stdin(
        {
            let mut command = ok();
            command
                .arg("mcp")
                .arg("serve")
                .arg("--repo")
                .arg(repo)
                .arg("--read-only");
            command
        },
        concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"repo_status","arguments":{}}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            "\n"
        ),
    );
    let responses = output
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        responses.len(),
        2,
        "the session must outlive the probe: {output}"
    );
    assert!(responses[1]["result"]["tools"].is_array(), "{output}");
    responses.into_iter().next().unwrap()
}

fn mcp_repo_status_error(repo: &std::path::Path) -> String {
    let response = mcp_repo_status(repo);
    assert_eq!(response["error"]["code"], -32000, "{response}");
    response["error"]["message"].as_str().unwrap().to_string()
}

/// One `tools/call` in a fresh `ok mcp serve` session, returning the response.
fn mcp_tool_call(
    repo: &std::path::Path,
    name: &str,
    arguments: serde_json::Value,
) -> serde_json::Value {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    });
    let output = run_with_stdin(
        {
            let mut command = ok();
            command
                .arg("mcp")
                .arg("serve")
                .arg("--repo")
                .arg(repo)
                .arg("--read-only");
            command
        },
        &format!("{request}\n"),
    );
    serde_json::from_str(output.lines().next().expect("the session answers")).unwrap()
}

/// The stderr of a command that must fail as a usage error, exit code 2.
fn run_usage_error(mut command: Command) -> String {
    let output = command.output().expect("command should run");
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected a usage error\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stderr).expect("stderr should be utf-8")
}

/// Each argument the caller got wrong exits 2 on the CLI and returns `-32602` on MCP, with the
/// same `invalid input: …` message where both surfaces take the same argument.
#[test]
fn caller_argument_errors_exit_2_and_return_invalid_params() {
    let (_temp, repo) = init_and_index_worker_repo();
    let cli = |args: &[&str]| {
        run_usage_error({
            let mut command = ok();
            command.arg("--repo").arg(&repo).args(args);
            command
        })
    };
    let mcp_error = |name: &str, arguments: serde_json::Value| {
        let response = mcp_tool_call(&repo, name, arguments);
        assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
        assert!(
            response["error"].get("data").is_none(),
            "{name}: {response}"
        );
        response["error"]["message"].as_str().unwrap().to_string()
    };

    // One blank-query message on both surfaces.
    let blank = format!(
        "invalid input: {}",
        open_kioku_storage::BLANK_SEARCH_QUERY_MESSAGE
    );
    for query in ["", "   "] {
        let stderr = cli(&["search", query]);
        assert!(stderr.contains(&blank), "{stderr}");
        assert_eq!(
            mcp_error("search_code", serde_json::json!({"query": query})),
            blank
        );
    }

    // A query the caller never sent is a different mistake from one sent empty. `ok search`
    // takes the query as a positional argument, so clap reports the missing one; `search_code`
    // reports it the way every other tool reports a forgotten argument.
    let missing = "invalid input: missing required string argument `query`";
    let stderr = cli(&["search"]);
    assert!(stderr.contains("QUERY"), "{stderr}");
    assert!(!stderr.contains(&blank), "{stderr}");
    for arguments in [serde_json::json!({}), serde_json::json!({"query": 7})] {
        assert_eq!(mcp_error("search_code", arguments), missing);
    }

    let stderr = cli(&["retrieve-context", "bogus"]);
    assert!(
        stderr.contains("invalid input: no context handle `bogus`"),
        "{stderr}"
    );
    let message = mcp_error("retrieve_context", serde_json::json!({"handle": "bogus"}));
    assert!(
        message.starts_with("invalid input: no context handle `bogus`"),
        "{message}"
    );

    // A contract id Open Kioku never issued for this repository, like an unknown handle.
    for args in [
        vec!["contract", "show", "missing"],
        vec![
            "contract",
            "verify",
            "--id",
            "missing",
            "--changed",
            "src/lib.rs",
        ],
    ] {
        let stderr = cli(&args);
        assert!(
            stderr.contains("invalid input: no contract `missing` is stored"),
            "{args:?}: {stderr}"
        );
    }
    let message = mcp_error(
        "verify_change",
        serde_json::json!({"contract_id": "missing", "changed_files": ["src/lib.rs"]}),
    );
    assert!(
        message.starts_with("invalid input: no contract `missing` is stored"),
        "{message}"
    );

    for (name, arguments) in [
        ("repo_status", serde_json::json!({"detail": "everything"})),
        (
            "plan_change",
            serde_json::json!({"task": "change Worker::run", "detail": "everything"}),
        ),
    ] {
        let message = mcp_error(name, arguments);
        assert!(message.starts_with("invalid input: "), "{name}: {message}");
    }

    // A plan the caller supplies and the contract builder rejects: no primary context and no
    // allowed files.
    let planned = mcp_tool_call(
        &repo,
        "plan_change",
        serde_json::json!({"task": "change Worker::run", "format": "json"}),
    );
    let mut plan = planned["result"]["structuredContent"].clone();
    assert!(plan.is_object(), "{planned}");
    plan["primary_context"] = serde_json::json!([]);
    plan["recommended_change_boundary"]["allowed_files"] = serde_json::json!([]);
    let plan_json = plan.to_string();
    let refused = mcp_error(
        "plan_change",
        serde_json::json!({"persist": true, "store": false, "plan_json": plan_json}),
    );
    assert!(
        refused.starts_with("invalid input: contract generation requires"),
        "{refused}"
    );
    let stderr = cli(&[
        "contract",
        "create",
        "--plan-json",
        &plan_json,
        "--no-store",
    ]);
    assert!(stderr.contains(&refused), "{stderr}");

    // JSON arguments that do not decode.
    let stderr = cli(&["contract", "create", "--plan-json", "{", "--no-store"]);
    assert!(
        stderr.contains("invalid input: --plan-json is malformed"),
        "{stderr}"
    );
    let message = mcp_error(
        "plan_change",
        serde_json::json!({"persist": true, "store": false, "plan_json": "{"}),
    );
    assert!(
        message.starts_with("invalid input: `plan_json` is malformed"),
        "{message}"
    );
    let not_a_plan = repo.join("not-a-plan.json");
    fs::write(&not_a_plan, "{}").unwrap();
    let stderr = cli(&[
        "verify",
        "--plan",
        not_a_plan.to_str().unwrap(),
        "--changed",
        "src/lib.rs",
    ]);
    assert!(stderr.contains("is not a valid saved plan"), "{stderr}");
    let message = mcp_error(
        "verify_change",
        serde_json::json!({"plan_json": "{}", "changed_files": ["src/lib.rs"]}),
    );
    assert!(
        message.starts_with("invalid input: `plan_json` is malformed"),
        "{message}"
    );
    let stderr = cli(&[
        "contract",
        "verify",
        "--contract-json",
        "{",
        "--changed",
        "src/lib.rs",
    ]);
    assert!(
        stderr.contains("invalid input: contract JSON is malformed"),
        "{stderr}"
    );
    let message = mcp_error(
        "verify_change",
        serde_json::json!({"contract_json": "{", "changed_files": ["src/lib.rs"]}),
    );
    assert!(
        message.starts_with("invalid input: `contract_json` is malformed"),
        "{message}"
    );

    // A regex that does not parse: the same message on both surfaces.
    let message = mcp_error("regex_search", serde_json::json!({"pattern": "pub fn ("}));
    assert!(
        message.starts_with("invalid input: regex parse error"),
        "{message}"
    );
    let stderr = cli(&["search", "--regex", "pub fn ("]);
    assert!(stderr.contains(&message), "{stderr}");

    // Unknown enumerated values and missing arguments; clap rejects the CLI equivalents.
    for (name, arguments) in [
        (
            "search_code",
            serde_json::json!({"query": "Worker", "mode": "fuzzy"}),
        ),
        (
            "get_references",
            serde_json::json!({"query": "Worker", "kind": "callsites"}),
        ),
        ("get_definition", serde_json::json!({})),
    ] {
        let message = mcp_error(name, arguments);
        assert!(message.starts_with("invalid input: "), "{name}: {message}");
    }
}

/// A manifest `ok watch` withdrew after a failed incremental update: the rows are still there,
/// so every surface says why, rather than describing a repository nobody has indexed.
#[test]
fn withdrawn_manifest_reports_its_reason_on_cli_and_mcp() {
    let (_temp, repo) = init_and_index_worker_repo();
    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    let reason = "an incremental update replaced the changed files' rows and then failed";
    open_kioku_storage_sqlite::SqliteStore::open(&db)
        .unwrap()
        .withdraw_manifest(reason)
        .unwrap();

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("status");
        command
    });
    assert!(
        status.contains("repository is not indexed") && status.contains(reason),
        "{status}"
    );

    let status_json: serde_json::Value = serde_json::from_str(&run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    }))
    .unwrap();
    assert_eq!(status_json["indexed"], false, "{status_json}");
    assert_eq!(status_json["reason"], reason, "{status_json}");

    let (doctor, _stderr) = run_failure({
        let mut command = ok();
        command.arg("doctor").arg(&repo);
        command
    });
    assert!(doctor.contains(reason), "{doctor}");

    let response = mcp_repo_status(&repo);
    assert_eq!(
        response["result"]["structuredContent"]["reason"], reason,
        "{response}"
    );
}

/// `ok path` and MCP `dependency_path` resolve their arguments through one function, so the
/// same names give the same route on both surfaces.
#[test]
fn path_and_dependency_path_resolve_the_same_nodes() {
    let (_temp, repo) = init_and_index_worker_repo();
    for (from, to, from_kind) in [
        ("src/lib.rs", "Worker", "file:"),
        ("Worker", "run", "symbol:"),
    ] {
        let cli: serde_json::Value = serde_json::from_str(&run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(&repo)
                .arg("--json")
                .args(["path", from, to]);
            command
        }))
        .unwrap();
        let response = mcp_tool_call(
            &repo,
            "dependency_path",
            serde_json::json!({"from": from, "to": to}),
        );
        let mcp = &response["result"]["structuredContent"];
        assert!(
            mcp["from"].as_str().unwrap().starts_with(from_kind),
            "{response}"
        );
        assert_eq!(mcp["edges"], cli, "{from} -> {to}: {response}");
    }

    // An endpoint the index does not hold is a repository lookup that found nothing, as an
    // unknown symbol name is: `-32000` and exit 1, with one message on both surfaces.
    let response = mcp_tool_call(
        &repo,
        "dependency_path",
        serde_json::json!({"from": "src/missing.rs", "to": "Worker"}),
    );
    assert_eq!(response["error"]["code"], -32000, "{response}");
    let unresolved = response["error"]["message"].as_str().unwrap();
    assert!(
        unresolved.contains("`src/missing.rs` is not an indexed file path"),
        "{unresolved}"
    );
    let output = ok()
        .arg("--repo")
        .arg(&repo)
        .args(["path", "src/missing.rs", "Worker"])
        .output()
        .expect("command should run");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(unresolved), "{stderr}");
}

/// What the lock holder prints once it holds the lock.
const INDEX_LOCK_HELD: &str = "ok-test: index lock held";
/// The line that tells the lock holder to release the lock as a finishing writer does.
const INDEX_LOCK_RELEASE: &str = "release";

/// Holds the index writer lock from a separate process for the tests that need a live writer,
/// which re-run this test binary through [`IndexLockHolder::spawn`] with
/// `OK_TEST_HOLD_INDEX_LOCK` naming the repository. Without the variable there is nothing to
/// hold and it returns at once.
///
/// Only an explicit release line takes the normal release path, which removes the lock file.
/// Stdin closing without one means the parent went away, and the holder exits with an error
/// status and no destructors, so a holder that stops early is never mistaken for one that was
/// released or killed on purpose (#478).
#[test]
fn hold_index_lock_for_a_parent_test() {
    let Some(repo) = std::env::var_os("OK_TEST_HOLD_INDEX_LOCK") else {
        return;
    };
    let _lock = open_kioku_storage::generations::IndexWriteLock::acquire(
        std::path::Path::new(&repo),
        std::time::Duration::from_secs(10),
    )
    .expect("the parent test leaves the lock free");
    // Straight to the stdout handle: the harness captures `println!`, not this.
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{INDEX_LOCK_HELD}")
        .and_then(|()| stdout.flush())
        .expect("the parent reads the readiness line");
    drop(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => {
                eprintln!("ok-test: stdin closed before the parent released the index lock");
                std::process::exit(3);
            }
            Ok(_) if line.trim_end() == INDEX_LOCK_RELEASE => return,
            Ok(_) => eprintln!("ok-test: ignoring unexpected holder input {line:?}"),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                eprintln!("ok-test: reading the release line failed: {error}");
                std::process::exit(3);
            }
        }
    }
}

/// A separate process holding a repository's index writer lock. [`Self::release`] releases it
/// as a finishing writer does; [`Self::kill`] releases it as Ctrl-C or the OOM killer would.
/// Both first prove the holder is still alive, and every failure carries its output.
struct IndexLockHolder {
    child: std::process::Child,
    /// The holder's stdout and stderr, as files so nothing blocks on a pipe the holder fills.
    output_dir: tempfile::TempDir,
}

impl IndexLockHolder {
    /// Returns once the holder has said it holds the lock and the lock reads as held.
    fn spawn(repo: &std::path::Path) -> Self {
        let output_dir = tempfile::tempdir().unwrap();
        let stdout = fs::File::create(output_dir.path().join("stdout")).unwrap();
        let stderr = fs::File::create(output_dir.path().join("stderr")).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "hold_index_lock_for_a_parent_test",
                "--exact",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("OK_TEST_HOLD_INDEX_LOCK", repo)
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .expect("the lock holder should spawn");
        let mut holder = Self { child, output_dir };
        let waiting_since = std::time::Instant::now();
        while !holder.stdout().contains(INDEX_LOCK_HELD) {
            holder.assert_alive("before taking the lock");
            assert!(
                waiting_since.elapsed() < std::time::Duration::from_secs(60),
                "the lock holder never took the lock\n{}",
                holder.output()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            open_kioku_storage::generations::index_write_in_progress(repo),
            "the holder says it holds the lock, but the lock reads as free\n{}",
            holder.output()
        );
        holder
    }

    fn stdout(&self) -> String {
        fs::read_to_string(self.output_dir.path().join("stdout")).unwrap_or_default()
    }

    fn output(&self) -> String {
        let stderr = fs::read_to_string(self.output_dir.path().join("stderr")).unwrap_or_default();
        format!(
            "holder stdout:\n{}\nholder stderr:\n{stderr}",
            self.stdout()
        )
    }

    fn assert_alive(&mut self, when: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("the lock holder exited {when}: {status}\n{}", self.output());
        }
    }

    /// Releases the lock the way a finishing `ok index` does: the holder drops it and exits.
    fn release(&mut self) {
        self.assert_alive("before it was released");
        let mut stdin = self.child.stdin.take().expect("the holder's stdin is open");
        writeln!(stdin, "{INDEX_LOCK_RELEASE}").unwrap();
        drop(stdin);
        let status = self.child.wait().unwrap();
        assert!(
            status.success(),
            "the lock holder failed to release: {status}\n{}",
            self.output()
        );
    }

    /// Kills the holder, so no destructor runs, and proves the kill is what ended it.
    fn kill(&mut self) {
        self.assert_alive("before it was killed");
        // `Child::wait` closes the child's stdin before waiting, and a closed stdin is the
        // holder's cue to stop. SIGKILL is not instantaneous on macOS: a holder thread woken by
        // that EOF could still run, and before this helper existed it took the normal release
        // path and deleted the lock file the test then required (#478). Stdin stays open until
        // the holder has been reaped, so the kill is the only thing that can end it.
        let stdin = self.child.stdin.take();
        self.child.kill().unwrap();
        let status = self.child.wait().unwrap();
        drop(stdin);
        #[cfg(unix)]
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&status),
            Some(9),
            "the lock holder must end by SIGKILL, not exit on its own: {status}\n{}",
            self.output()
        );
        #[cfg(not(unix))]
        assert!(
            !status.success(),
            "the lock holder must end by the kill, not exit on its own: {status}\n{}",
            self.output()
        );
    }
}

impl Drop for IndexLockHolder {
    /// A parent test that fails with the holder alive does not leave it running.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// `.ok/index.lock` means "indexing in progress" only while a live process holds it. A file
/// with no holder — what Ctrl-C, an OOM kill, or a crash during `ok index` leaves — is ignored
/// by every read surface and taken over by the next `ok index`, instead of wedging reads until
/// a person deletes it.
#[test]
fn index_lock_reports_in_progress_only_while_a_live_process_holds_it() {
    use open_kioku_storage::generations::{
        index_lock_path, index_write_in_progress, indexing_in_progress_message,
    };
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().canonicalize().unwrap();
    let repo = repo.as_path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub struct Worker;\n").unwrap();
    let lock_path = index_lock_path(repo);
    let expected = indexing_in_progress_message(repo);
    assert!(expected.starts_with("indexing in progress"), "{expected}");
    let json_status = |repo: &std::path::Path| -> serde_json::Value {
        let status = run({
            let mut command = ok();
            command.arg("--repo").arg(repo).arg("--json").arg("status");
            command
        });
        serde_json::from_str(&status).unwrap()
    };

    // A lock file nobody holds: the repository is simply unindexed.
    fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    fs::write(&lock_path, b"").unwrap();
    assert_eq!(json_status(repo)["indexed"], false);
    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command.arg("--repo").arg(repo).args(["search", "Worker"]);
        command
    });
    assert!(stderr.contains("repository is not indexed"), "{stderr}");
    assert_eq!(
        mcp_repo_status(repo)["result"]["structuredContent"]["indexed"],
        false
    );

    // A live process holds it: every surface says the index is being built.
    let mut holder = IndexLockHolder::spawn(repo);
    for args in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["search", "Worker"],
        vec!["impact", "--file", "src/lib.rs"],
    ] {
        holder.assert_alive("while the read surfaces were probed");
        let (_stdout, stderr) = run_failure({
            let mut command = ok();
            command.arg("--repo").arg(repo).args(&args);
            command
        });
        assert!(
            stderr.contains(&expected),
            "{args:?}: {stderr}\n{}",
            holder.output()
        );
        assert!(
            !stderr.contains("repository is not indexed"),
            "{args:?} must not call an index being built unindexed: {stderr}\n{}",
            holder.output()
        );
    }
    holder.assert_alive("while the read surfaces were probed");
    let (doctor, _stderr) = run_failure({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(
        doctor.contains("[fail] index") && doctor.contains(&expected),
        "{doctor}"
    );
    assert!(
        doctor.contains("Wait for the running `ok index`"),
        "{doctor}"
    );
    assert!(mcp_repo_status_error(repo).contains(&expected));

    // Killed, as Ctrl-C or the OOM killer would: no destructor runs, so the file stays, and
    // the kernel has released the lock anyway.
    holder.kill();
    assert!(
        lock_path.exists(),
        "a killed writer leaves its lock file\n{}",
        holder.output()
    );
    assert!(!index_write_in_progress(repo));
    assert_eq!(json_status(repo)["indexed"], false);
    assert_eq!(
        mcp_repo_status(repo)["result"]["structuredContent"]["indexed"],
        false
    );
    let started = std::time::Instant::now();
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(25),
        "`ok index` must take over an unheld lock file, not wait out the 30 s writer timeout"
    );
    assert_eq!(json_status(repo)["indexed"], true);
}

/// The manifest is written after the graph and the search index, so a run that fails between
/// them publishes nothing: the repository reads as unindexed, not as an index whose graph or
/// search side is half of the previous one.
#[test]
fn index_run_that_fails_after_the_rows_publishes_no_manifest() {
    let (_temp, repo) = init_and_index_worker_repo();
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], true, "{status}");

    // A regular file where the search index directory goes fails the search stage, which
    // runs after the rows and the graph are written.
    let search_dir = open_kioku_storage::generations::resolve_index_location(&repo).tantivy_dir();
    fs::remove_dir_all(&search_dir).unwrap();
    fs::write(&search_dir, b"not a directory").unwrap();
    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });
    assert!(stderr.contains("Not a directory"), "{stderr}");
    assert!(
        !open_kioku_storage::generations::index_write_in_progress(&repo),
        "a failed run releases the writer lock"
    );

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], false, "{status}");
    assert!(status["message"]
        .as_str()
        .unwrap()
        .starts_with("repository is not indexed"));
    let response = mcp_repo_status(&repo);
    assert_eq!(
        response["result"]["structuredContent"]["indexed"], false,
        "{response}"
    );

    // Repaired, the next run publishes again. The failed run adopted the legacy layout into
    // a generation directory (a move, before the stage that failed), so resolve the path anew.
    let search_dir = open_kioku_storage::generations::resolve_index_location(&repo).tantivy_dir();
    fs::remove_file(&search_dir).unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], true, "{status}");
}

/// `ok snapshot import` is a writer: it waits for a live writer's lock like `ok index` does,
/// and until it has the lock the published index stays exactly as it was.
#[test]
fn snapshot_import_waits_for_a_live_index_writer_and_leaves_the_index_untouched() {
    let (_temp, repo) = init_and_index_worker_repo();
    // Imports relate the artifact's commit to HEAD, so the repository is under Git.
    commit_all(&repo, "initial");
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "export", "--quality", "fast"]);
        command
    });
    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    let index_before = fs::read(&db).unwrap();

    let mut holder = IndexLockHolder::spawn(&repo);
    let mut import = {
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .args(["snapshot", "import"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.spawn().expect("snapshot import should spawn")
    };
    // Far inside the 30 s writer wait, and far longer than an import of this fixture takes.
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(
        import.try_wait().unwrap().is_none(),
        "snapshot import must wait while a live writer holds the lock"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        index_before,
        "a waiting import must not touch the index"
    );
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["indexed"], true,
        "the previous index stays published while the import waits: {status}"
    );

    holder.release();
    let output = import.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.contains("waiting for exclusive index writer lock"),
        "{stderr}"
    );
    let imported: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(imported["imported"], true, "{imported}");
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], true, "{status}");
}

/// The imported database is moved into place without its manifest, and the manifest is put
/// back only after the search index is rebuilt: an import that fails at the search stage
/// leaves the repository unindexed, not a published manifest over a missing search index.
#[test]
fn snapshot_import_that_fails_at_the_search_stage_publishes_no_manifest() {
    let (_temp, repo) = init_and_index_worker_repo();
    // Imports relate the artifact's commit to HEAD, so the repository is under Git.
    commit_all(&repo, "initial");
    let import = |repo: &std::path::Path| {
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .args(["snapshot", "import"]);
        command
    };
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "export", "--quality", "fast"]);
        command
    });

    // A regular file where the search index directory goes fails the search rebuild, which
    // runs after the imported database has replaced the previous one.
    let search_dir = open_kioku_storage::generations::resolve_index_location(&repo).tantivy_dir();
    fs::remove_dir_all(&search_dir).unwrap();
    fs::write(&search_dir, b"not a directory").unwrap();
    let (_stdout, stderr) = run_failure(import(&repo));
    assert!(stderr.contains("Not a directory"), "{stderr}");
    assert!(
        stderr.contains("the imported index was not published"),
        "{stderr}"
    );

    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let count = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert!(count("files") > 0, "the imported rows are in place");
        assert_eq!(
            count("manifests"),
            0,
            "no manifest may be published before the search index is rebuilt"
        );
    }
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], false, "{status}");
    assert!(status["message"]
        .as_str()
        .unwrap()
        .starts_with("repository is not indexed"));
    let response = mcp_repo_status(&repo);
    assert_eq!(
        response["result"]["structuredContent"]["indexed"], false,
        "{response}"
    );

    // Repaired, the next writer takes the lock without waiting for a retry, and the next
    // import publishes over the index it builds.
    fs::remove_file(&search_dir).unwrap();
    let (_stdout, index_stderr) = run_ok_with_stderr({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });
    let lock_wait = index_stderr
        .lines()
        .find_map(|line| line.strip_prefix("index[lock] acquired index writer lock, elapsed="))
        .and_then(|rest| rest.strip_suffix('s'))
        .and_then(|seconds| seconds.parse::<f64>().ok())
        .unwrap_or_else(|| panic!("`ok index` did not report taking the lock: {index_stderr}"));
    assert!(
        lock_wait < 0.25,
        "`ok index` after the failed import must take the lock on its first attempt (a retry \
         waits 250 ms): {index_stderr}"
    );
    let imported: serde_json::Value = serde_json::from_str(&run(import(&repo))).unwrap();
    assert_eq!(imported["imported"], true, "{imported}");
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], true, "{status}");
    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["--json", "search", "Worker"]);
        command
    });
    assert!(search.contains("src/lib.rs"), "{search}");
}

/// A replaced index whose manifest cannot be read for a reason other than not being an index
/// (here a damaged schema entry for `manifests`) aborts the import before the file is moved:
/// the previous index and its manifest stay where they are.
#[test]
fn snapshot_import_aborts_when_the_replaced_index_cannot_be_read() {
    let (_temp, repo) = init_and_index_worker_repo();
    // Imports relate the artifact's commit to HEAD, so the repository is under Git.
    commit_all(&repo, "initial");
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "export", "--quality", "fast"]);
        command
    });
    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    let original_schema: String = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'manifests'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // `writable_schema` also lets this connection load the schema while the entry is damaged,
    // which is how the entry is put back below.
    let set_manifests_schema = |sql: &str| {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA writable_schema = ON;").unwrap();
        conn.execute(
            "UPDATE sqlite_master SET sql = ?1 WHERE type = 'table' AND name = 'manifests'",
            [sql],
        )
        .unwrap();
    };
    set_manifests_schema("CREATE TABLE manifests(");

    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "import"]);
        command
    });
    assert!(
        stderr.contains("before replacing it; it was left in place"),
        "{stderr}"
    );
    assert!(db.is_file(), "the previous index stays at its path");
    let leftovers = |dir: std::path::PathBuf, prefix: &str| -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(prefix))
            .collect()
    };
    assert!(
        leftovers(repo.join(".ok"), ".index.sqlite.").is_empty(),
        "the previous index was not moved aside"
    );
    assert!(
        leftovers(repo.join(".ok/artifacts"), ".index.snapshot.import.").is_empty(),
        "the decompressed import is removed"
    );

    set_manifests_schema(&original_schema);
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["indexed"], true,
        "the previous manifest is still published: {status}"
    );
}

/// Export reads the index like every other read surface: an index a live writer has not
/// published is refused as being built, and a database with no manifest is not exported.
#[test]
fn snapshot_export_refuses_an_unpublished_index() {
    let (_temp, repo) = init_and_index_worker_repo();
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");
    let export = |repo: &std::path::Path| {
        run_failure({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .args(["snapshot", "export", "--quality", "fast"]);
            command
        })
        .1
    };

    let mut holder = IndexLockHolder::spawn(&repo);
    // What a full `ok index` leaves between staging its rows and publishing the manifest.
    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute("DELETE FROM manifests", [])
        .unwrap();
    let stderr = export(&repo);
    let expected = open_kioku_storage::generations::indexing_in_progress_message(&repo);
    assert!(stderr.contains(&expected), "{stderr}");
    assert!(!metadata_path.exists(), "nothing is exported mid-index");

    holder.release();
    let stderr = export(&repo);
    assert!(stderr.contains("repository is not indexed"), "{stderr}");
    assert!(!metadata_path.exists());
}

/// A running MCP session holds the database an import replaces. The import withdraws that
/// database's manifest before moving it aside, so the session probes the index path again and
/// answers from the imported index instead of from the file that was replaced.
#[test]
fn snapshot_import_moves_a_running_mcp_session_to_the_imported_index() {
    let (_temp, repo) = init_and_index_worker_repo();
    // Imports relate the artifact's commit to HEAD, so the repository is under Git.
    commit_all(&repo, "initial");
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "export", "--quality", "fast"]);
        command
    });
    let exported: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(repo.join(".ok/artifacts/index.snapshot.json")).unwrap(),
    )
    .unwrap();
    fs::write(repo.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
    let reindexed: serde_json::Value = serde_json::from_str(&run({
        let mut command = ok();
        command.arg("--json").arg("index").arg(&repo);
        command
    }))
    .unwrap();
    assert_ne!(
        reindexed["file_count"], exported["file_count"],
        "the live index must differ from the snapshot for this test to see which one is served"
    );

    let mut server = {
        let mut command = ok();
        command
            .args(["mcp", "serve", "--read-only", "--repo"])
            .arg(&repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command.spawn().expect("the MCP server should spawn")
    };
    let mut stdin = server.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(server.stdout.take().unwrap());
    let mut served_file_count = |id: u64| -> serde_json::Value {
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"repo_status","arguments":{{}}}}}}"#
        )
        .unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut stdout, &mut line).unwrap();
        let response: serde_json::Value = serde_json::from_str(&line).unwrap();
        response["result"]["structuredContent"]["file_count"].clone()
    };
    assert_eq!(served_file_count(1), reindexed["file_count"]);

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["snapshot", "import"]);
        command
    });
    assert_eq!(
        served_file_count(2),
        exported["file_count"],
        "the session must serve the imported index, not the database it replaced"
    );

    drop(stdin);
    server.wait().unwrap();
}

const EXPORT_LEDGER_ROWS_PER_BATCH: i64 = 10;
const EXPORT_LEDGER_KEPT_BATCHES: i64 = 200;

/// A thread committing ledger batches into `db` until `stop` is set: each transaction inserts
/// a whole batch, deletes the oldest kept one and updates a totals row, so a copy mixing two
/// committed states breaks a relation [`assert_export_ledger_is_one_committed_state`] checks.
fn spawn_export_ledger_writer(
    db: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    committed: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> std::thread::JoinHandle<rusqlite::Result<()>> {
    use std::sync::atomic::Ordering;
    let mut conn = rusqlite::Connection::open(&db).unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    conn.execute_batch(
        "CREATE TABLE export_ledger (
           batch INTEGER NOT NULL, slot INTEGER NOT NULL, payload BLOB NOT NULL,
           PRIMARY KEY (batch, slot));
         CREATE TABLE export_ledger_totals (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           batches INTEGER NOT NULL, rows INTEGER NOT NULL);
         INSERT INTO export_ledger_totals VALUES (1, 0, 0);
         PRAGMA synchronous = OFF;
         PRAGMA wal_autocheckpoint = 16;",
    )
    .unwrap();
    std::thread::spawn(move || {
        let mut batch = 0_i64;
        while !stop.load(Ordering::SeqCst) {
            let tx = conn.transaction()?;
            for slot in 0..EXPORT_LEDGER_ROWS_PER_BATCH {
                tx.execute(
                    "INSERT INTO export_ledger (batch, slot, payload) \
                     VALUES (?1, ?2, zeroblob(1024))",
                    rusqlite::params![batch, slot],
                )?;
            }
            let deleted = tx.execute(
                "DELETE FROM export_ledger WHERE batch = ?1",
                rusqlite::params![batch - EXPORT_LEDGER_KEPT_BATCHES],
            )? as i64;
            tx.execute(
                "UPDATE export_ledger_totals \
                 SET batches = batches + 1 - ?1, rows = rows + ?2 - ?3",
                rusqlite::params![
                    i64::from(deleted > 0),
                    EXPORT_LEDGER_ROWS_PER_BATCH,
                    deleted
                ],
            )?;
            tx.commit()?;
            committed.fetch_add(1, Ordering::SeqCst);
            batch += 1;
        }
        Ok(())
    })
}

fn assert_export_ledger_is_one_committed_state(db: &std::path::Path) {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let partial_batches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT batch FROM export_ledger GROUP BY batch \
             HAVING COUNT(*) <> ?1)",
            [EXPORT_LEDGER_ROWS_PER_BATCH],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(partial_batches, 0, "a batch was exported partway through");
    let (batches, rows, span): (i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(DISTINCT batch), COUNT(*), \
             COALESCE(MAX(batch) - MIN(batch) + 1, 0) FROM export_ledger",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let totals: (i64, i64) = conn
        .query_row(
            "SELECT batches, rows FROM export_ledger_totals",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(totals, (batches, rows), "totals from another commit");
    assert_eq!(span, batches, "the kept batches are not one window");
    assert!(rows > 0, "the ledger was exported empty");
}

/// `--quality fast` exports one committed state while another connection keeps committing
/// and checkpointing: every artifact expands to a database that passes `integrity_check`,
/// holds whole ledger batches with matching totals, and carries its manifest, and the last
/// one imports and serves search.
#[test]
fn snapshot_export_fast_copies_one_committed_state_while_a_writer_commits() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc;
    let (temp, repo) = init_and_index_worker_repo();
    // Imports relate the artifact's commit to HEAD, so the repository is under Git.
    commit_all(&repo, "initial");
    let db = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    let stop = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicU64::new(0));
    let writer = spawn_export_ledger_writer(db.clone(), Arc::clone(&stop), Arc::clone(&committed));
    let waiting_since = std::time::Instant::now();
    while committed.load(Ordering::SeqCst) < EXPORT_LEDGER_KEPT_BATCHES as u64 {
        assert!(
            !writer.is_finished(),
            "the writer stopped before filling the ledger"
        );
        assert!(waiting_since.elapsed() < std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let expanded = temp.path().join("expanded.sqlite");
    for _ in 0..3 {
        let before = committed.load(Ordering::SeqCst);
        let exported: serde_json::Value = serde_json::from_str(&run({
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("--json").args([
                "snapshot",
                "export",
                "--quality",
                "fast",
            ]);
            command
        }))
        .unwrap();
        assert!(
            committed.load(Ordering::SeqCst) > before,
            "the writer must commit while the export runs for this test to mean anything"
        );
        assert_eq!(exported["quality"], "fast", "{exported}");
        assert_eq!(exported["metadata"]["compression_level"], 1, "{exported}");

        let _ = fs::remove_file(&expanded);
        zstd::stream::copy_decode(
            fs::File::open(repo.join(".ok/artifacts/index.snapshot.zst")).unwrap(),
            fs::File::create(&expanded).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fs::metadata(&expanded).unwrap().len(),
            exported["metadata"]["original_size_bytes"]
                .as_u64()
                .unwrap()
        );
        assert_export_ledger_is_one_committed_state(&expanded);
        let manifests: i64 = rusqlite::Connection::open(&expanded)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM manifests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(manifests, 1);
    }
    stop.store(true, Ordering::SeqCst);
    writer.join().unwrap().unwrap();

    let imported: serde_json::Value = serde_json::from_str(&run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .args(["snapshot", "import"]);
        command
    }))
    .unwrap();
    assert_eq!(imported["imported"], true, "{imported}");
    assert_export_ledger_is_one_committed_state(&db);
    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .args(["--json", "search", "Worker"]);
        command
    });
    assert!(search.contains("src/lib.rs"), "{search}");
}

fn bump_manifest_schema_version(db_path: &std::path::Path) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let raw: String = conn
        .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
    manifest["schema_version"] =
        serde_json::json!(open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION + 1);
    manifest["from_a_newer_open_kioku"] = serde_json::json!(true);
    conn.execute(
        "UPDATE manifests SET json = ?1 WHERE id = 1",
        rusqlite::params![manifest.to_string()],
    )
    .unwrap();
}

/// An index whose manifest was written by a newer Open Kioku is refused everywhere with one
/// sentence naming the two ways out, on the live index and on a snapshot artifact alike.
#[test]
fn index_from_a_newer_open_kioku_reports_upgrade_or_reindex_on_every_surface() {
    let (_temp, repo) = init_and_index_worker_repo();
    let expected = open_kioku_storage_sqlite::newer_index_message(
        open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION + 1,
    );

    // A snapshot exported by this version, rewritten as a newer version would have written
    // its manifest: the import refuses it and leaves the live index alone.
    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg("fast");
        command
    });
    let artifact_path = repo.join(".ok/artifacts/index.snapshot.zst");
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");
    let newer_db = repo.join("newer.sqlite");
    zstd::stream::copy_decode(
        fs::File::open(&artifact_path).unwrap(),
        fs::File::create(&newer_db).unwrap(),
    )
    .unwrap();
    bump_manifest_schema_version(&newer_db);
    zstd::stream::copy_encode(
        fs::File::open(&newer_db).unwrap(),
        fs::File::create(&artifact_path).unwrap(),
        3,
    )
    .unwrap();
    let mut metadata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    metadata["original_size_bytes"] = serde_json::json!(fs::metadata(&newer_db).unwrap().len());
    metadata["compressed_size_bytes"] =
        serde_json::json!(fs::metadata(&artifact_path).unwrap().len());
    fs::write(&metadata_path, metadata.to_string()).unwrap();
    let index_path = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    let live_index = fs::read(&index_path).unwrap();
    let (_stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains(&expected), "{stderr}");
    assert_eq!(fs::read(&index_path).unwrap(), live_index);

    // The live index, rewritten the same way.
    bump_manifest_schema_version(&index_path);
    for args in [
        vec!["status"],
        vec!["--json", "status"],
        vec!["search", "Worker"],
        vec!["symbol", "definition", "Worker"],
        vec!["snapshot", "export"],
    ] {
        let (_stdout, stderr) = run_failure({
            let mut command = ok();
            command.arg("--repo").arg(&repo).args(&args);
            command
        });
        assert!(stderr.contains(&expected), "{args:?}: {stderr}");
    }
    let (doctor, _stderr) = run_failure({
        let mut command = ok();
        command.arg("doctor").arg(&repo);
        command
    });
    assert!(
        doctor.contains("[fail] index") && doctor.contains(&expected),
        "{doctor}"
    );
    assert!(mcp_repo_status_error(&repo).contains(&expected));
    assert_eq!(
        mcp_repo_status(&repo)["error"]["data"]["state"],
        "index_newer_than_binary"
    );

    // `ok index` is one of the two ways out.
    run({
        let mut command = ok();
        command.arg("index").arg(&repo);
        command
    });
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(&repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["indexed"], true, "{status}");
    assert_eq!(
        status["schema_version"],
        open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION
    );
}

/// Credential-shaped test values are assembled at run time so that no string in the repository
/// matches a real provider's key format or reads as a leaked secret to a scanner.
fn striding_token(alphabet: &[u8], len: usize, stride: usize, offset: usize) -> String {
    (0..len)
        .map(|index| char::from(alphabet[(index * stride + offset) % alphabet.len()]))
        .collect()
}

/// Every stored field value and every indexed term of the lexical index. Stored fields are
/// compressed on disk, so a byte scan of the index files could not prove a value is absent.
fn tantivy_stored_texts_and_terms(index_dir: &std::path::Path) -> Vec<String> {
    use tantivy::schema::Value;
    let index = tantivy::Index::open_in_dir(index_dir).unwrap();
    let schema = index.schema();
    let searcher = index.reader().unwrap().searcher();
    let mut texts = Vec::new();
    for segment in searcher.segment_readers() {
        let store = segment.get_store_reader(1).unwrap();
        for document in store.iter::<tantivy::TantivyDocument>(segment.alive_bitset()) {
            for (_, value) in document.unwrap().field_values() {
                texts.extend(value.as_str().map(str::to_string));
            }
        }
        for (field, entry) in schema.fields() {
            if !entry.is_indexed() {
                continue;
            }
            let inverted = segment.inverted_index(field).unwrap();
            let mut terms = inverted.terms().stream().unwrap();
            while terms.advance() {
                texts.push(String::from_utf8_lossy(terms.key()).into_owned());
            }
        }
    }
    texts
}

fn assert_secrets_absent(label: &str, haystack: &str, secrets: &[&str]) {
    let lowered = haystack.to_ascii_lowercase();
    for secret in secrets {
        assert!(
            !lowered.contains(&secret.to_ascii_lowercase()),
            "{label} holds a secret value"
        );
    }
}

/// A config file's secret-like values never reach any store or output, and the file stays
/// indexed and searchable by its keys (#379).
#[test]
fn config_secret_values_never_reach_the_index_search_snapshot_or_mcp() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    // Shaped like a cloud access key id without any provider's prefix, and a token under a
    // key no rule names, so only the entropy rule can catch it.
    let cloud_key = format!(
        "{}{}",
        ["OK", "CK"].concat(),
        striding_token(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", 16, 7, 3)
    );
    let token = striding_token(
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        32,
        17,
        5,
    );
    let secrets = [cloud_key.as_str(), token.as_str()];
    // Named for a secret but not key material, so it is indexed with its values redacted.
    let config_path = "config/secrets.yaml";
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("config")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn load_settings() {}\n").unwrap();
    fs::write(
        repo.join(config_path),
        format!(
            "storage:\n  provider: objectstore\n  access_key_id: {cloud_key}\nwebhooks:\n  delivery_nonce: \"{token}\"\n"
        ),
    )
    .unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    let indexed = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(
        indexed.contains(
            "redaction: 1 data, config, or prose file(s) indexed with secret-like values replaced by [REDACTED]"
        ),
        "{indexed}"
    );

    // Semantic target text and embeddings are built from the same redacted chunks, here with
    // the local hashing provider, which downloads nothing.
    run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("semantic").arg("index");
        command
    });
    let semantic_targets = fs::read_to_string(repo.join(".ok/vectors/current/ids.json")).unwrap();
    assert!(
        semantic_targets.contains(config_path),
        "the config file is in the semantic corpus"
    );
    let semantic_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("--semantic")
            .arg("access_key_id");
        command
    });
    assert_secrets_absent("ok search --semantic", &semantic_search, &secrets);

    // SQLite, its WAL, and the vector store's target and embedding files hold text
    // uncompressed, so their bytes are checked directly.
    for entry in walkdir::WalkDir::new(repo.join(".ok")) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let bytes = fs::read(entry.path()).unwrap();
            assert_secrets_absent(
                &entry.path().display().to_string(),
                &String::from_utf8_lossy(&bytes),
                &secrets,
            );
        }
    }
    let lexical =
        tantivy_stored_texts_and_terms(&open_kioku_search_tantivy::default_index_dir(repo));
    assert!(
        lexical.iter().any(|text| text.contains("access_key_id")),
        "the lexical index holds the config file's keys"
    );
    assert_secrets_absent("tantivy", &lexical.join("\n"), &secrets);

    let by_key = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("access_key_id");
        command
    });
    assert!(by_key.contains(config_path), "{by_key}");
    assert!(by_key.contains("[REDACTED]"), "{by_key}");
    assert_secrets_absent("ok search by key", &by_key, &secrets);
    // One sentence from one function: the human surface reports the same redaction state as
    // the agent surface, checked against the MCP response below.
    let by_key_json: serde_json::Value = serde_json::from_str(&by_key).unwrap();
    let cli_caveat = by_key_json["caveats"]
        .as_array()
        .expect("ok search --json carries caveats")
        .iter()
        .find_map(|caveat| {
            let caveat = caveat.as_str()?;
            caveat.contains("[REDACTED]").then(|| caveat.to_string())
        })
        .unwrap_or_else(|| panic!("ok search reports the redaction caveat: {by_key}"));
    // Derived from the function every surface renders, not copied: a wording change cannot
    // leave this green against stale text.
    let expected_caveat = open_kioku_core::redaction_search_caveat(Some(1))
        .expect("one redacted file produces a caveat");
    assert_eq!(
        cli_caveat, expected_caveat,
        "`ok search --json` renders the shared caveat sentence"
    );

    // The default surface, in whichever form it renders: `output` prints pretty JSON when the
    // payload is under 4 KiB and the human text only above it, so this asserts the caveat
    // reaches the representation this run produced rather than assuming which one that is.
    // `search_text_rendering_prints_the_redaction_caveat` covers the human path.
    let by_key_text = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("search")
            .arg("access_key_id");
        command
    });
    let caveat_reaches_default_output =
        match serde_json::from_str::<serde_json::Value>(&by_key_text) {
            Ok(value) => value["caveats"].as_array().is_some_and(|caveats| {
                caveats
                    .iter()
                    .any(|caveat| caveat.as_str() == Some(expected_caveat.as_str()))
            }),
            Err(_) => by_key_text.contains(&format!("caveat: {expected_caveat}")),
        };
    assert!(
        caveat_reaches_default_output,
        "`ok search` carries the redaction caveat on its default output: {by_key_text}"
    );
    assert_secrets_absent("ok search default output", &by_key_text, &secrets);
    for secret in secrets {
        let by_value = run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .arg("--json")
                .arg("search")
                .arg(secret);
            command
        });
        assert_secrets_absent("ok search by value", &by_value, &secrets);
    }

    for quality in ["best", "fast"] {
        let exported = run({
            let mut command = ok();
            command
                .arg("--repo")
                .arg(repo)
                .arg("--json")
                .arg("snapshot")
                .arg("export")
                .arg("--quality")
                .arg(quality);
            command
        });
        assert_secrets_absent("snapshot export report", &exported, &secrets);
        let artifact = fs::File::open(repo.join(".ok/artifacts/index.snapshot.zst")).unwrap();
        let database = zstd::decode_all(artifact).unwrap();
        assert_secrets_absent(
            &format!("snapshot artifact ({quality})"),
            &String::from_utf8_lossy(&database),
            &secrets,
        );
    }

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    assert_secrets_absent("ok status", &status, &secrets);
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["quality"]["redacted_files"], 1, "{status}");

    let doctor = run({
        let mut command = ok();
        command.arg("--json").arg("doctor").arg(repo);
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "redaction")
        .expect("doctor has a redaction check");
    assert_eq!(check["status"], "pass", "{check}");
    assert!(
        check["message"]
            .as_str()
            .unwrap()
            .starts_with("1 data, config, or prose file(s)"),
        "{check}"
    );

    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"access_key_id"}}}"#.to_string(),
        format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{{"name":"search_code","arguments":{{"query":"{cloud_key}"}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"search_code","arguments":{{"query":"{token}"}}}}}}"#
        ),
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"repo_status","arguments":{}}}"#.to_string(),
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"access_key_id","mode":"hybrid"}}}"#.to_string(),
    ];
    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &format!("{}\n", requests.join("\n")),
    );
    assert_secrets_absent("mcp", &mcp, &secrets);
    let responses = mcp
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let by_id = |id: u64| {
        responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap_or_else(|| panic!("no MCP response {id}: {mcp}"))
    };
    assert!(by_id(1).to_string().contains(config_path), "{mcp}");
    assert!(by_id(5).to_string().contains(config_path), "{mcp}");
    assert!(
        mcp.contains(&cli_caveat),
        "search_code reports the same redaction caveat as `ok search`: {mcp}"
    );
    assert_eq!(
        by_id(4)["result"]["structuredContent"]["quality"]["redacted_files"],
        1,
        "{mcp}"
    );
}

/// An index written before secret-value redaction held config values as read. Replacing its
/// rows leaves those bytes in SQLite free pages, so the first index run over it compacts the
/// database after publishing (#379).
#[test]
fn indexing_over_an_index_written_before_redaction_drops_its_unredacted_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let value = striding_token(
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        32,
        17,
        5,
    );
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("config")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn load() {}\n").unwrap();
    fs::write(
        repo.join("config/app.yaml"),
        format!("service:\n  api_token: {value}\n"),
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    // Rewind the database to what an earlier release left: the value as read, in more rows
    // than the index that replaces them, under a manifest with no redaction count.
    let database = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    {
        let conn = rusqlite::Connection::open(&database).unwrap();
        let file_id: String = conn
            .query_row(
                "SELECT id FROM files WHERE path LIKE '%app.yaml'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let stale = format!("  api_token: {value}\n").repeat(400);
        conn.execute(
            "INSERT INTO chunks(id, file_id, start_line, end_line, text, json) \
             VALUES('pre-redaction', ?1, 1, 400, ?2, ?3)",
            rusqlite::params![
                file_id,
                stale,
                serde_json::json!({ "text": stale }).to_string()
            ],
        )
        .unwrap();
        let manifest: String = conn
            .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        manifest["quality"]
            .as_object_mut()
            .unwrap()
            .remove("redacted_files");
        conn.execute(
            "UPDATE manifests SET json = ?1 WHERE id = 1",
            [manifest.to_string()],
        )
        .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
    }
    // A vector store built before redaction holds the same values in its target text.
    let vectors_root = open_kioku_storage::generations::resolve_index_location(repo).vectors_root();
    fs::create_dir_all(vectors_root.join("current")).unwrap();
    let stale_vectors = vectors_root.join("current/ids.json");
    fs::write(&stale_vectors, format!("[{{\"text\": \"{value}\"}}]")).unwrap();

    // Whatever file under `.ok` still holds the value, named. A path captured before the run
    // is the wrong instrument here: `ok index` migrates a legacy `.ok/index.sqlite` into a
    // generation directory the first time it runs over one, so checking the old path after the
    // move reports "the bytes are gone" because the file moved, not because it was compacted.
    let holder = |needle: &str| -> Option<std::path::PathBuf> {
        walkdir::WalkDir::new(repo.join(".ok"))
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .find(|entry| {
                fs::read(entry.path())
                    .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
                    .unwrap_or(false)
            })
            .map(|entry| entry.path().to_path_buf())
    };
    assert!(
        holder(&value).is_some(),
        "the fixture holds the value as an earlier release stored it"
    );
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert!(status["quality"]["redacted_files"].is_null(), "{status}");

    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    if let Some(path) = holder(&value) {
        panic!(
            "a file under .ok still holds the value stored before redaction: {}",
            path.display()
        );
    }
    // Re-resolved, because the run may have moved the index into a generation directory. It
    // must exist: an assertion against a database that is simply absent would pass for the
    // wrong reason, which is the failure this test previously had.
    let database = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    assert!(
        database.exists(),
        "the published index database is where this run left it"
    );
    assert!(
        walkdir::WalkDir::new(repo.join(".ok"))
            .into_iter()
            .filter_map(|entry| entry.ok())
            .all(|entry| entry.file_name() != "ids.json"),
        "the vector store built before redaction is discarded"
    );
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["quality"]["redacted_files"], 1, "{status}");
    assert_eq!(
        status["quality"]["pending_pre_redaction_compaction"], false,
        "a completed clearing is recorded as done: {status}"
    );

    // An index whose clearing did not finish keeps the work in its manifest, so `ok doctor`
    // reports it and the next run retries it rather than treating it as done.
    {
        let conn = rusqlite::Connection::open(&database).unwrap();
        let manifest: String = conn
            .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        manifest["quality"]["pending_pre_redaction_compaction"] = serde_json::Value::Bool(true);
        conn.execute(
            "UPDATE manifests SET json = ?1 WHERE id = 1",
            [manifest.to_string()],
        )
        .unwrap();
    }
    let doctor = run({
        let mut command = ok();
        command.arg("--json").arg("doctor").arg(repo);
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "redaction")
        .expect("doctor has a redaction check");
    assert_eq!(check["status"], "warn", "{check}");
    assert!(
        check["message"].as_str().unwrap().contains("outstanding"),
        "{check}"
    );

    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["quality"]["pending_pre_redaction_compaction"], false,
        "the next run retried the outstanding clearing: {status}"
    );
}

/// `ok snapshot export` is a distribution point, so it refuses what it cannot ship safely.
/// Two different states hide behind one predicate: an index that predates redaction holds the
/// values in live rows, which `VACUUM INTO` copies faithfully, so no mode is safe; one that has
/// been rebuilt owes only the clearing of free pages, which `--quality best` leaves behind and
/// `--quality fast` carries along with the file (#379).
#[test]
fn snapshot_export_refuses_the_states_it_cannot_ship_safely() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn load() {}\n").unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    let database = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    let rewrite_manifest = |mutate: &dyn Fn(&mut serde_json::Value)| {
        let conn = rusqlite::Connection::open(&database).unwrap();
        let stored: String = conn
            .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        let mut manifest: serde_json::Value = serde_json::from_str(&stored).unwrap();
        mutate(&mut manifest);
        conn.execute(
            "UPDATE manifests SET json = ?1 WHERE id = 1",
            [manifest.to_string()],
        )
        .unwrap();
    };
    let export = |quality: &str| {
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg(quality);
        command
    };

    // Written before redaction: the values are in rows, so every mode copies them.
    rewrite_manifest(&|manifest| {
        manifest["quality"]
            .as_object_mut()
            .unwrap()
            .remove("redacted_files");
    });
    for quality in ["best", "fast"] {
        let (_stdout, stderr) = run_failure(export(quality));
        assert!(
            stderr.contains("was written before secret-value redaction"),
            "{quality}: {stderr}"
        );
    }

    // Rebuilt, but the clearing of free pages is still owed: `best` rewrites the database and
    // leaves them behind, `fast` copies the file as it is.
    rewrite_manifest(&|manifest| {
        let quality = manifest["quality"].as_object_mut().unwrap();
        quality.insert("redacted_files".into(), serde_json::json!(0));
        quality.insert(
            "pending_pre_redaction_compaction".into(),
            serde_json::Value::Bool(true),
        );
    });
    let (_stdout, stderr) = run_failure(export("fast"));
    assert!(stderr.contains("still owes the clearing"), "{stderr}");
    let exported = run(export("best"));
    let exported: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(exported["ok"], true, "{exported}");
}

/// `output` renders the human form only when the pretty JSON exceeds 4 KiB, so the caveat loop
/// on the ranked search path is reachable only on a large result set. A small repository takes
/// the JSON branch instead, which is why the end-to-end test could not cover this (#379).
#[test]
fn search_text_rendering_prints_the_redaction_caveat() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("config")).unwrap();
    let value = striding_token(
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        32,
        17,
        5,
    );
    for index in 0..20 {
        fs::write(
            repo.join(format!("config/service{index}.yaml")),
            format!("service: svc{index}\naccess_key_id: {value}\n"),
        )
        .unwrap();
    }
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    // The count comes from the index rather than from this test's arithmetic, so the expected
    // sentence is the one the code would render for this repository.
    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    let redacted = status["quality"]["redacted_files"].as_u64().unwrap();
    let expected_caveat = open_kioku_core::redaction_search_caveat(Some(redacted as usize))
        .expect("a redacted repository produces a caveat");

    let text = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("search")
            .arg("access_key_id");
        command
    });
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_err(),
        "this result set is large enough to take the human rendering: {text}"
    );
    assert!(
        text.contains(&format!("caveat: {expected_caveat}")),
        "the human rendering prints the redaction caveat: {text}"
    );
    assert!(!text.contains(value.as_str()), "{text}");
}

/// A TypeScript repository with `src/rates.ts` and, when given, `src/rates.test.ts`.
fn rates_repo(test_file: Option<&str>) -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("package.json"),
        "{\"name\":\"rates\",\"version\":\"0.1.0\",\"scripts\":{\"test\":\"vitest\"}}\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/rates.ts"),
        "export function convertCurrency(amount: number, rate: number): number {\n  return Math.round(amount * rate);\n}\n",
    )
    .unwrap();
    if let Some(body) = test_file {
        fs::write(repo.join("src/rates.test.ts"), body).unwrap();
    }
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    temp
}

/// `ok --json tests` and MCP `find_tests_for_change` for one path, which must be one answer.
fn test_selection_on_both_surfaces(repo: &std::path::Path, path: &str) -> serde_json::Value {
    let cli = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("tests")
            .arg("--changed")
            .arg(path);
        command
    });
    let cli: serde_json::Value = serde_json::from_str(&cli).unwrap();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "find_tests_for_change", "arguments": {"path": path}},
    });
    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &request.to_string(),
    );
    let mcp: serde_json::Value = serde_json::from_str(mcp.trim()).unwrap();
    let mcp = &mcp["result"]["structuredContent"];
    for field in ["excluded", "excluded_sample", "caveats"] {
        assert_eq!(mcp[field], cli[field], "{field}: cli {cli} mcp {mcp}");
    }
    let names = |value: &serde_json::Value| {
        value["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|test| test["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(mcp), names(&cli));
    cli
}

fn setup_audit_provider(repo: &std::path::Path, name: &str) -> serde_json::Value {
    let audit = run({
        let mut command = ok();
        command.arg("--json").arg("setup").arg("audit").arg(repo);
        command
    });
    let audit: serde_json::Value = serde_json::from_str(&audit).unwrap();
    audit["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("no `{name}` provider in {audit}"))
}

/// #494: a file whose tests are all skipped is not a file with no tests. `ok tests`,
/// `find_tests_for_change` and the setup audit say so in the words the context pack uses, and
/// none of them advises indexing test files that are already indexed.
#[test]
fn every_surface_tells_skipped_tests_from_absent_ones() {
    let temp = rates_repo(Some(
        "test.skip(\"rounds half up\", () => {});\n\ntest.todo(\"handles negative rates\");\n",
    ));
    let repo = temp.path();
    const PACK_CAVEAT: &str = "every indexed test target is a disabled test the runner skips";

    let selection = test_selection_on_both_surfaces(repo, "src/rates.ts");
    assert_eq!(selection["tests"], serde_json::json!([]), "{selection}");
    assert_eq!(selection["excluded"]["disabled"], 2, "{selection}");
    assert_eq!(
        selection["excluded_sample"].as_array().map(Vec::len),
        Some(2),
        "{selection}"
    );
    let caveat = selection["caveats"][0].as_str().unwrap();
    assert!(
        caveat.starts_with("2 indexed test target(s) found for `src/rates.ts`, all excluded"),
        "{caveat}"
    );
    assert!(
        caveat.contains("disabled test the runner skips"),
        "{caveat}"
    );

    let tests = setup_audit_provider(repo, "tests");
    let evidence = tests["evidence"].as_str().unwrap();
    assert!(
        evidence.contains("excluded: 2 disabled test the runner skips"),
        "{tests}"
    );
    let next_step = tests["next_step"].as_str().unwrap();
    assert!(!next_step.contains("Index test files"), "{tests}");
    assert!(next_step.starts_with("Enable the skipped tests"), "{tests}");
    let validation = setup_audit_provider(repo, "validation");
    assert!(
        validation["evidence"]
            .as_str()
            .unwrap()
            .ends_with(PACK_CAVEAT),
        "{validation}"
    );

    // The pack over the same index carries the same sentence.
    let pack = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("context")
            .arg("add tests for convertCurrency rounding");
        command
    });
    assert!(pack.contains(PACK_CAVEAT), "{pack}");

    let status = run({
        let mut command = ok();
        command.arg("--repo").arg(repo).arg("--json").arg("status");
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["quality"]["test_count"], 0);
    assert_eq!(status["quality"]["excluded_test_targets"]["disabled"], 2);
}

#[test]
fn a_file_with_no_tests_reads_as_none_found() {
    let temp = rates_repo(None);
    let repo = temp.path();

    let selection = test_selection_on_both_surfaces(repo, "src/rates.ts");
    assert_eq!(selection["tests"], serde_json::json!([]), "{selection}");
    assert!(selection.get("excluded").is_none(), "{selection}");
    assert_eq!(
        selection["caveats"],
        serde_json::json!(["no indexed test target was found for `src/rates.ts`"])
    );
    let tests = setup_audit_provider(repo, "tests");
    assert_eq!(tests["evidence"], "0 indexed test target(s)");
    assert_eq!(
        tests["next_step"],
        "Index test files before relying on validation recommendations."
    );
}

#[test]
fn a_file_with_runnable_and_skipped_tests_recommends_one_and_counts_the_other() {
    let temp = rates_repo(Some(
        "import { convertCurrency } from \"./rates\";\n\ntest(\"rounds half up\", () => {\n  expect(convertCurrency(2, 1.5)).toBe(3);\n});\n\ntest.skip(\"handles negative rates\", () => {});\n",
    ));
    let repo = temp.path();

    let selection = test_selection_on_both_surfaces(repo, "src/rates.ts");
    let names = selection["tests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|test| test["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(names.contains(&"rounds half up"), "{selection}");
    assert!(!names.contains(&"handles negative rates"), "{selection}");
    assert_eq!(selection["excluded"]["disabled"], 1, "{selection}");
    assert_eq!(
        selection["excluded_sample"][0]["name"], "handles negative rates",
        "{selection}"
    );
    assert!(selection.get("caveats").is_none(), "{selection}");
    let tests = setup_audit_provider(repo, "tests");
    assert!(tests["next_step"].is_null(), "{tests}");

    // An empty page is not an empty match: `limit: 0` must not claim the file has no
    // runnable test.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "find_tests_for_change",
            "arguments": {"path": "src/rates.ts", "limit": 0},
        },
    });
    let page = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &request.to_string(),
    );
    let page: serde_json::Value = serde_json::from_str(page.trim()).unwrap();
    let page = &page["result"]["structuredContent"];
    assert_eq!(page["tests"], serde_json::json!([]), "{page}");
    let caveat = page["caveats"][0].as_str().unwrap();
    assert!(
        caveat.starts_with("1 runnable test target(s) matched `src/rates.ts`"),
        "{caveat}"
    );
}

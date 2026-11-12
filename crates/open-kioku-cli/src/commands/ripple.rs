/// `ok ripple`: the call sites a change left untouched.
fn handle_ripple_command(json: bool, repo: &Path, args: RippleArgs) -> anyhow::Result<()> {
    let repo = gate_command_repo(repo)?;
    // Status 1 is "a site is broken" under `--fail-on-broken`; a run that could not be made
    // is 2, so a CI step can tell them apart.
    let report = match ripple_report(&repo, Some(args.base), args.files, !args.no_index) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("ok ripple: {error:#}");
            std::process::exit(2);
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.to_text());
    }
    if args.fail_on_broken && report.broken_sites > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// The ripple of the working tree against `base`, reading the repository's index when there
/// is one and `use_index` allows it. An index that cannot be opened is the same as none.
///
/// `repo` may be a directory inside a git working tree (a project of a monorepo): the tree is
/// read from its top, where a caller of a changed function may be, and `only_files` are
/// relative to `repo`. An index is one of `repo`, with paths relative to it, so it is read only
/// when `repo` is the top.
fn ripple_report(
    repo: &Path,
    base: Option<String>,
    only_files: Vec<PathBuf>,
    use_index: bool,
) -> anyhow::Result<open_kioku_patch::ripple::RippleReport> {
    let top = gate_git_toplevel(repo).unwrap_or_else(|| repo.to_path_buf());
    let same = match (top.canonicalize(), repo.canonicalize()) {
        (Ok(top), Ok(repo)) => top == repo,
        _ => top == repo,
    };
    let only_files = if same {
        only_files
    } else {
        let prefix = repo
            .canonicalize()
            .ok()
            .zip(top.canonicalize().ok())
            .and_then(|(repo, top)| repo.strip_prefix(top).map(Path::to_path_buf).ok())
            .unwrap_or_default();
        only_files
            .into_iter()
            .map(|path| prefix.join(path))
            .collect()
    };
    let store = if use_index && same {
        SqliteStore::open_repo_index(repo).ok().flatten()
    } else {
        None
    };
    let input = open_kioku_patch::ripple::RippleInput { base, only_files };
    Ok(open_kioku_patch::ripple::ripple(
        &top,
        store.as_ref().map(|store| store as &dyn OkStore),
        &input,
    )?)
}

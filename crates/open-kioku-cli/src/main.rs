#[cfg(feature = "mem-profile")]
#[global_allocator]
static ALLOCATOR: open_kioku_cli::mem_profile::CountingAllocator =
    open_kioku_cli::mem_profile::CountingAllocator;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Clap intercepts `--version` before subcommand handling, so honor the
    // machine-readable `ok --version --json` combination during initial
    // argument parsing.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if open_kioku_cli::try_print_version_json(&args) {
        return Ok(());
    }
    let result = open_kioku_cli::run_cli().await;
    #[cfg(feature = "mem-profile")]
    open_kioku_cli::mem_profile::report();
    if let Err(err) = &result {
        // Caller-input errors exit like clap usage errors (2), distinct from the runtime
        // failures anyhow reports with 1, so scripts can tell a bad invocation from a bad repo.
        if open_kioku_cli::is_invalid_input_error(err) {
            eprintln!("Error: {err:#}");
            std::process::exit(2);
        }
    }
    result
}

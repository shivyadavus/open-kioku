use open_kioku_actions::PolicyGate;
use open_kioku_config::OkConfig;
use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub async fn run_allowlisted(
    config: &OkConfig,
    command: &str,
    timeout_secs: u64,
) -> Result<CommandOutput> {
    PolicyGate::new(config).ensure_command_allowed(command)?;
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| OkError::PolicyDenied("empty command".into()))?;
    let args = parts.collect::<Vec<_>>();
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(output) => {
            let output = output?;
            Ok(CommandOutput {
                status: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                timed_out: false,
            })
        }
        Err(_) => Ok(CommandOutput {
            status: None,
            stdout: String::new(),
            stderr: "command timed out".into(),
            timed_out: true,
        }),
    }
}

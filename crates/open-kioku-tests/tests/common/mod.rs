//! Where the `ok` binary is, for tests of a package that does not build it.
//!
//! Cargo sets `CARGO_BIN_EXE_ok` only for the tests of the package that declares the binary.
//! `assert_cmd::Command::cargo_bin` used to make up for that by looking beside the test
//! executable, which a toolchain that keeps its build directory apart from its target
//! directory no longer allows. This looks in the same place first and then asks Cargo.

use std::path::PathBuf;
use std::process::Command as Process;
use std::sync::OnceLock;

fn ok_binary() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_ok") {
            return PathBuf::from(path);
        }
        let name = format!("ok{}", std::env::consts::EXE_SUFFIX);
        // `target/<profile>/deps/<test>` has the binary one directory up.
        let beside = std::env::current_exe().ok().and_then(|test| {
            test.ancestors()
                .skip(1)
                .take(4)
                .map(|directory| directory.join(&name))
                .find(|candidate| candidate.is_file())
        });
        if let Some(path) = beside {
            return path;
        }
        // Cargo says where it put it; the binary is already built when the workspace's tests
        // run, so this compiles nothing.
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let output = Process::new(cargo)
            .args([
                "build",
                "--package",
                "open-kioku-cli",
                "--bin",
                "ok",
                "--message-format=json",
            ])
            .output()
            .expect("cargo should run to locate the `ok` binary");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|message| message["reason"] == "compiler-artifact")
            .filter(|message| message["target"]["name"] == "ok")
            .filter_map(|message| message["executable"].as_str().map(PathBuf::from))
            .next_back()
            .unwrap_or_else(|| {
                panic!(
                    "the `ok` binary was not found beside the tests and `cargo build` did not \
                     name it: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
    })
}

/// A command that runs the workspace's `ok` binary.
pub fn ok_command() -> assert_cmd::Command {
    assert_cmd::Command::new(ok_binary())
}

//! `cargo xtask <build|run|test> [--release] [--graphics wgpu|vulkan|opengl] [-- ARGS...]`
//!
//! Thin dispatcher so the common invocations are one short command.
use std::process::{Command, ExitCode};

const USAGE: &str =
    "usage: cargo xtask <build|run|test> [--release] [--graphics wgpu|vulkan|opengl] [-- ARGS...]";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(task) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };

    let mut release = false;
    let mut passthrough = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--release" => release = true,
            "--graphics" | "-g" => {
                passthrough.push("--graphics".to_owned());
                passthrough.extend(args.next());
            }
            "--" => passthrough.extend(args.by_ref()),
            other => passthrough.push(other.to_owned()),
        }
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut command = Command::new(cargo);
    match task.as_str() {
        "build" => command.args(["build", "-p", "shocora_egui"]),
        "run" => command.args(["run", "-p", "shocora_egui"]),
        "test" => command.args(["test", "--workspace"]),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if release {
        command.arg("--release");
    }
    if task == "run" && !passthrough.is_empty() {
        command.arg("--").args(&passthrough);
    }

    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("failed to run cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

//! `lb-ext` — the developer CLI for Lazybones extensions.
//!
//! The out-of-tree replacement for `make publish-ext`. It fronts the same build/pack/publish path
//! the in-shell Extension Studio drives (lb's `lb-devkit`): `new` scaffolds, `build` compiles,
//! `pack` produces a signed Artifact, `publish` POSTs it to a node's `/extensions`.
//!
//! The command *dispatch* and CLI surface are real here; each command's body calls into `lb-devkit`
//! once that library is published from lb (ext-sdk-scope.md). Until then the bodies print the exact
//! action they will take, so the surface is usable and testable without pretending to be finished.

mod command;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match command::run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(command::CliError::Usage(msg)) => {
            eprintln!("{msg}\n\n{}", command::USAGE);
            ExitCode::from(2)
        }
        Err(command::CliError::Failed(msg)) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

//! The `animated2d` developer CLI.

#![forbid(unsafe_code)]

use std::io::Write;
use std::process::ExitCode;

use a2d_cli::{args, commands, Command};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let command = match args::parse(argv) {
        Ok(command) => command,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };

    let result = match command {
        Command::Help => {
            let _ = out.write_all(args::HELP.as_bytes());
            return ExitCode::SUCCESS;
        }
        Command::Version => {
            let _ = writeln!(out, "animated2d {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Command::Inspect { input, game } => commands::inspect(&mut out, &input, game.as_deref()),
        Command::Import { input, output, game } => commands::import(&mut out, &input, &output, game.as_deref()),
        Command::Validate { package } => match commands::validate(&mut out, &package) {
            // A package that loads but has warnings is a distinct outcome from
            // one that fails to load, so it gets its own exit code.
            Ok(true) => return ExitCode::SUCCESS,
            Ok(false) => return ExitCode::from(1),
            Err(e) => Err(e),
        },
        Command::Preview { package, out: frames, exit_after } => {
            commands::preview(&mut out, &package, frames.as_deref(), exit_after.map(std::time::Duration::from_secs_f32))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = out.flush();
            eprintln!("error: {e}");
            ExitCode::from(3)
        }
    }
}

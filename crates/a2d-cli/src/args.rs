//! Argument parsing.
//!
//! Hand-rolled rather than pulled from a crate: four subcommands and three
//! flags is not enough surface to justify the dependency, and it keeps the CLI
//! buildable offline.

use std::fmt;
use std::path::PathBuf;

/// A parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Inspect { input: PathBuf, game: Option<String> },
    Import { input: PathBuf, output: PathBuf, game: Option<String> },
    Validate { package: PathBuf },
    Preview { package: PathBuf },
    Help,
    Version,
}

/// Why a command line could not be understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgError {
    pub message: String,
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn err(message: impl Into<String>) -> ArgError {
    ArgError { message: message.into() }
}

/// Parses arguments, excluding the program name.
pub fn parse<I, S>(args: I) -> Result<Command, ArgError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let Some(first) = args.first() else { return Ok(Command::Help) };

    match first.as_str() {
        "help" | "--help" | "-h" => return Ok(Command::Help),
        "version" | "--version" | "-V" => return Ok(Command::Version),
        _ => {}
    }

    let rest = &args[1..];
    match first.as_str() {
        "inspect" => {
            let (positional, game, output) = split_flags(rest)?;
            if output.is_some() {
                return Err(err("`inspect` does not take -o/--output"));
            }
            Ok(Command::Inspect { input: one_path(positional, "inspect <input>")?, game })
        }
        "import" => {
            let (positional, game, output) = split_flags(rest)?;
            let input = one_path(positional, "import <input> -o <output.a2dpack>")?;
            let output = output.ok_or_else(|| err("`import` needs -o/--output <output.a2dpack>"))?;
            Ok(Command::Import { input, output, game })
        }
        "validate" => {
            let (positional, game, output) = split_flags(rest)?;
            reject_extra(game, output)?;
            Ok(Command::Validate { package: one_path(positional, "validate <package>")? })
        }
        "preview" => {
            let (positional, game, output) = split_flags(rest)?;
            reject_extra(game, output)?;
            Ok(Command::Preview { package: one_path(positional, "preview <package>")? })
        }
        other if other.starts_with('-') => Err(err(format!("unknown option `{other}`; try `animated2d help`"))),
        other => Err(err(format!("unknown command `{other}`; try `animated2d help`"))),
    }
}

type Split = (Vec<String>, Option<String>, Option<PathBuf>);

fn split_flags(args: &[String]) -> Result<Split, ArgError> {
    let mut positional = Vec::new();
    let mut game = None;
    let mut output = None;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-o" | "--output" => {
                let value = args.get(i + 1).ok_or_else(|| err("-o/--output needs a path"))?;
                output = Some(PathBuf::from(value));
                i += 2;
            }
            "--game" => {
                let value = args.get(i + 1).ok_or_else(|| err("--game needs a name"))?;
                game = Some(value.clone());
                i += 2;
            }
            _ if arg.starts_with("--game=") => {
                game = Some(arg["--game=".len()..].to_string());
                i += 1;
            }
            _ if arg.starts_with("--output=") => {
                output = Some(PathBuf::from(&arg["--output=".len()..]));
                i += 1;
            }
            // A lone `-` is a path, not a flag.
            _ if arg.starts_with('-') && arg.len() > 1 => {
                return Err(err(format!("unknown option `{arg}`")));
            }
            _ => {
                positional.push(arg.clone());
                i += 1;
            }
        }
    }
    Ok((positional, game, output))
}

fn one_path(positional: Vec<String>, usage: &str) -> Result<PathBuf, ArgError> {
    match positional.len() {
        1 => Ok(PathBuf::from(&positional[0])),
        0 => Err(err(format!("missing path; usage: animated2d {usage}"))),
        n => Err(err(format!("expected one path, got {n}; usage: animated2d {usage}"))),
    }
}

fn reject_extra(game: Option<String>, output: Option<PathBuf>) -> Result<(), ArgError> {
    if game.is_some() {
        return Err(err("this command does not take --game"));
    }
    if output.is_some() {
        return Err(err("this command does not take -o/--output"));
    }
    Ok(())
}

/// The `help` text.
pub const HELP: &str = "\
animated2d — Animated2D desktop viewer developer CLI

USAGE:
    animated2d <command> [options]

COMMANDS:
    inspect  <input>                        identify assets and report what would load
    import   <input> -o <out.a2dpack>       reconstruct a normalized package
    validate <package.a2dpack>              check a package for missing or unsupported data
    preview  <package.a2dpack>              open a package in the desktop viewer
    help                                    show this text
    version                                 show the version

OPTIONS:
    --game <name>    importer to use: generic, aeons_echo, depose_girls, nikke
                     (default: guessed from the assets present)
    -o, --output     destination package directory for `import`

NOTES:
    <input> may be a single asset or a directory. Detection reads file contents,
    not extensions, so `.skel.bytes` and `.atlas.txt` are handled directly.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Command {
        parse(args.iter().copied()).expect("should parse")
    }

    #[test]
    fn no_arguments_shows_help() {
        assert_eq!(parse(Vec::<String>::new()).unwrap(), Command::Help);
    }

    #[test]
    fn help_and_version_have_the_usual_spellings() {
        for spelling in ["help", "--help", "-h"] {
            assert_eq!(parse_ok(&[spelling]), Command::Help);
        }
        for spelling in ["version", "--version", "-V"] {
            assert_eq!(parse_ok(&[spelling]), Command::Version);
        }
    }

    #[test]
    fn inspect_takes_one_path() {
        assert_eq!(
            parse_ok(&["inspect", "hero.skel"]),
            Command::Inspect { input: PathBuf::from("hero.skel"), game: None }
        );
    }

    #[test]
    fn inspect_accepts_an_explicit_game() {
        assert_eq!(
            parse_ok(&["inspect", "assets/", "--game", "aeons_echo"]),
            Command::Inspect { input: PathBuf::from("assets/"), game: Some("aeons_echo".into()) }
        );
        assert_eq!(
            parse_ok(&["inspect", "assets/", "--game=nikke"]),
            Command::Inspect { input: PathBuf::from("assets/"), game: Some("nikke".into()) }
        );
    }

    #[test]
    fn import_needs_an_output() {
        assert_eq!(
            parse_ok(&["import", "hero.skel", "-o", "hero.a2dpack"]),
            Command::Import { input: PathBuf::from("hero.skel"), output: PathBuf::from("hero.a2dpack"), game: None }
        );
        let err = parse(["import", "hero.skel"]).unwrap_err();
        assert!(err.to_string().contains("-o/--output"), "{err}");
    }

    #[test]
    fn the_long_output_spelling_works_too() {
        assert_eq!(
            parse_ok(&["import", "a", "--output=b"]),
            Command::Import { input: PathBuf::from("a"), output: PathBuf::from("b"), game: None }
        );
    }

    #[test]
    fn flags_may_precede_the_path() {
        assert_eq!(
            parse_ok(&["import", "-o", "out", "in"]),
            Command::Import { input: PathBuf::from("in"), output: PathBuf::from("out"), game: None }
        );
    }

    #[test]
    fn validate_and_preview_take_one_path_and_no_flags() {
        assert_eq!(parse_ok(&["validate", "p.a2dpack"]), Command::Validate { package: "p.a2dpack".into() });
        assert_eq!(parse_ok(&["preview", "p.a2dpack"]), Command::Preview { package: "p.a2dpack".into() });
        assert!(parse(["validate", "p", "--game", "nikke"]).is_err());
    }

    #[test]
    fn inspect_rejects_an_output_flag() {
        let err = parse(["inspect", "a", "-o", "b"]).unwrap_err();
        assert!(err.to_string().contains("does not take"), "{err}");
    }

    #[test]
    fn a_missing_path_is_reported_with_usage() {
        let err = parse(["inspect"]).unwrap_err();
        assert!(err.to_string().contains("missing path"), "{err}");
        assert!(err.to_string().contains("animated2d inspect"), "{err}");
    }

    #[test]
    fn too_many_paths_are_reported() {
        let err = parse(["inspect", "a", "b"]).unwrap_err();
        assert!(err.to_string().contains("expected one path"), "{err}");
    }

    #[test]
    fn a_flag_without_its_value_is_reported() {
        assert!(parse(["import", "a", "-o"]).is_err());
        assert!(parse(["inspect", "a", "--game"]).is_err());
    }

    #[test]
    fn unknown_commands_and_options_are_reported() {
        assert!(parse(["frobnicate", "a"]).unwrap_err().to_string().contains("unknown command"));
        assert!(parse(["inspect", "--wat", "a"]).unwrap_err().to_string().contains("unknown option"));
    }

    #[test]
    fn help_text_documents_every_command() {
        for command in ["inspect", "import", "validate", "preview"] {
            assert!(HELP.contains(command), "help should mention {command}");
        }
        for game in ["generic", "aeons_echo", "depose_girls", "nikke"] {
            assert!(HELP.contains(game), "help should mention {game}");
        }
    }
}

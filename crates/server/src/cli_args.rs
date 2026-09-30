//! Argument handling shared by the server binaries. Configuration is
//! environment-driven; the only accepted arguments are the version and help flags.

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Run,
    Version,
    Help,
    Unknown(String),
}

/// Classifies the arguments after the program name.
pub fn classify<I: IntoIterator<Item = String>>(args: I) -> Invocation {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        None => Invocation::Run,
        Some("--version" | "-V") => Invocation::Version,
        Some("--help" | "-h") => Invocation::Help,
        Some(other) => Invocation::Unknown(other.to_string()),
    }
}

/// `<name> <version>`, the text `--version` prints.
pub fn version_line(name: &str, version: &str) -> String {
    format!("{name} {version}")
}

/// The short usage text for a binary that takes no arguments beyond the flags.
pub fn usage(name: &str, about: &str) -> String {
    format!(
        "{name}: {about}\n\nusage: {name} [--version | --help]\n\n\
         The node is configured through environment variables (see `avalon guide`), not flags."
    )
}

/// Handles the process arguments before any other work: prints and exits 0 for
/// `--version`/`--help`, prints usage to stderr and exits 2 for anything else.
pub fn handle_or_exit(name: &str, version: &str, about: &str) {
    match classify(std::env::args().skip(1)) {
        Invocation::Run => {}
        Invocation::Version => {
            println!("{}", version_line(name, version));
            std::process::exit(0);
        }
        Invocation::Help => {
            println!("{}", usage(name, about));
            std::process::exit(0);
        }
        Invocation::Unknown(arg) => {
            eprintln!(
                "{name}: unrecognized argument {arg:?}\n{}",
                usage(name, about)
            );
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_runs() {
        assert_eq!(classify(v(&[])), Invocation::Run);
    }

    #[test]
    fn version_and_help_flags() {
        assert_eq!(classify(v(&["--version"])), Invocation::Version);
        assert_eq!(classify(v(&["-V"])), Invocation::Version);
        assert_eq!(classify(v(&["--help"])), Invocation::Help);
        assert_eq!(classify(v(&["-h"])), Invocation::Help);
    }

    #[test]
    fn anything_else_is_unknown() {
        assert_eq!(
            classify(v(&["--port", "1"])),
            Invocation::Unknown("--port".to_string())
        );
        assert_eq!(
            classify(v(&["start"])),
            Invocation::Unknown("start".to_string())
        );
    }

    #[test]
    fn version_line_has_name_and_version() {
        assert_eq!(
            version_line("avalon-server", "1.2.3"),
            "avalon-server 1.2.3"
        );
    }
}

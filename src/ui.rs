//! Terminal status output.
//!
//! All status output (progress, warnings, step descriptions) is written to
//! **stderr**; stdout is reserved for command data so that output pipes
//! cleanly. The [`Reporter`] owns the verbosity policy — command code never
//! checks verbosity levels itself.

use crate::cli::GlobalArgs;

/// How much status output the user asked for.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Verbosity {
    /// `--quiet`: errors only.
    Quiet,
    /// The default: high-level status and warnings.
    Normal,
    /// `-v`: per-step status lines.
    Verbose,
    /// `-vv`: internal decisions (cache hits, resolved paths, resume choices).
    Debug,
    /// `-vvv`: full tracing, including HTTP requests with bodies redacted.
    Trace,
}

impl Verbosity {
    /// Derives the verbosity from the `--quiet` and `-v` flag counts.
    fn from_flags(quiet: bool, verbose: u8) -> Self {
        if quiet {
            Self::Quiet
        } else {
            match verbose {
                0 => Self::Normal,
                1 => Self::Verbose,
                2 => Self::Debug,
                _ => Self::Trace,
            }
        }
    }
}

/// Writes status output to stderr according to the requested [`Verbosity`].
#[derive(Debug)]
pub struct Reporter {
    /// The level every output decision is based on.
    verbosity: Verbosity,
}

impl Reporter {
    /// Builds a reporter from the global CLI flags.
    #[must_use]
    pub fn new(global: &GlobalArgs) -> Self {
        Self {
            verbosity: Verbosity::from_flags(global.quiet, global.verbose),
        }
    }

    /// Prints a warning; shown unless `--quiet` is set.
    pub fn warn(&self, message: &str) {
        if self.verbosity >= Verbosity::Normal {
            eprintln!("warning: {message}");
        }
    }

    /// Prints a `-v` status line describing the current step.
    pub fn detail(&self, message: &str) {
        if self.verbosity >= Verbosity::Verbose {
            eprintln!("{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the verbosity policy.

    #![expect(
        clippy::missing_panics_doc,
        reason = "tests panic on failure by design"
    )]

    use super::*;

    /// Flag combinations map to the expected verbosity levels.
    #[test]
    fn verbosity_from_flags() {
        assert_eq!(Verbosity::from_flags(true, 0), Verbosity::Quiet);
        assert_eq!(Verbosity::from_flags(false, 0), Verbosity::Normal);
        assert_eq!(Verbosity::from_flags(false, 1), Verbosity::Verbose);
        assert_eq!(Verbosity::from_flags(false, 2), Verbosity::Debug);
        assert_eq!(Verbosity::from_flags(false, 3), Verbosity::Trace);
        assert_eq!(Verbosity::from_flags(false, 9), Verbosity::Trace);
    }
}

//! Terminal status output.
//!
//! All status output (progress, warnings, step descriptions) is written to
//! **stderr**; stdout is reserved for command data so that output pipes
//! cleanly. The [`Reporter`] owns the verbosity policy — command code never
//! checks verbosity levels itself.

use crate::cli::GlobalArgs;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

/// Installs the `tracing-subscriber` fmt layer, writing structured log lines
/// to stderr at the given maximum level.
fn install_tracing(level: tracing::Level) {
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .init();
}

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
    /// Whether animated progress UI (spinners, bars) may be drawn.
    progress: bool,
    /// Whether stderr is a terminal a human can answer prompts on.
    interactive: bool,
}

impl Reporter {
    /// Builds a reporter from the global CLI flags; progress UI is disabled
    /// by `--no-progress` and whenever stderr is not a terminal.
    #[must_use]
    pub fn new(global: &GlobalArgs) -> Self {
        let verbosity = Verbosity::from_flags(global.quiet, global.verbose);
        match verbosity {
            Verbosity::Debug => install_tracing(tracing::Level::DEBUG),
            Verbosity::Trace => install_tracing(tracing::Level::TRACE),
            _ => {}
        }
        let terminal = std::io::stderr().is_terminal();
        Self {
            verbosity,
            progress: !global.no_progress && terminal && verbosity < Verbosity::Debug,
            interactive: terminal,
        }
    }

    /// Whether stderr is a terminal that can answer prompts.
    #[must_use]
    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Asks a yes/no confirmation on stderr (defaulting to "no").
    ///
    /// `assume_yes` (the `--yes` flag) short-circuits to `true`; in a
    /// non-interactive session without `--yes` this refuses to guess.
    ///
    /// # Errors
    ///
    /// Fails when the session is non-interactive and `--yes` was not given,
    /// or when the prompt itself fails.
    pub fn confirm(&self, prompt: &str, assume_yes: bool) -> anyhow::Result<bool> {
        if assume_yes {
            return Ok(true);
        }
        if !self.interactive {
            anyhow::bail!("not an interactive session; pass --yes to confirm: {prompt}");
        }
        Ok(dialoguer::Confirm::new()
            .with_prompt(prompt)
            .default(false)
            .interact()?)
    }

    /// Prints a status line; shown unless `--quiet` is set.
    pub fn status(&self, message: &str) {
        if self.verbosity >= Verbosity::Normal {
            eprintln!("{message}");
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

    /// Starts a spinner with the given message on stderr.
    ///
    /// When progress UI is disabled (`--no-progress`, `--quiet`, or stderr
    /// is not a terminal) the message is printed as a plain status line and
    /// a hidden bar is returned, so callers never need to special-case it.
    pub fn spinner(&self, message: &str) -> ProgressBar {
        if self.progress && self.verbosity >= Verbosity::Normal {
            let bar = ProgressBar::new_spinner().with_message(message.to_owned());
            bar.enable_steady_tick(Duration::from_millis(120));
            bar
        } else {
            self.status(message);
            ProgressBar::hidden()
        }
    }

    /// Creates the progress UI for a batch of parallel downloads.
    #[must_use]
    pub fn downloads(&self) -> Downloads {
        Downloads {
            multi: (self.progress && self.verbosity >= Verbosity::Normal).then(MultiProgress::new),
        }
    }

    /// Prints the end-of-run summary table of a mutating command on stderr,
    /// so it never pollutes pipeable stdout data.
    pub fn summary(&self, header: &[&str], rows: &[Vec<String>]) {
        if self.verbosity >= Verbosity::Normal {
            for line in render_table(header, rows) {
                eprintln!("{line}");
            }
        }
    }
}

/// Progress UI for a batch of parallel downloads (one bar per download
/// under a shared `MultiProgress`).
#[derive(Debug)]
pub struct Downloads {
    /// The multi-bar manager; `None` when progress UI is disabled.
    multi: Option<MultiProgress>,
}

impl Downloads {
    /// Adds a progress bar for one download, labelled with `name`. Hidden
    /// (but fully functional) when progress UI is disabled.
    #[must_use]
    pub fn bar(&self, name: &str) -> ProgressBar {
        self.multi
            .as_ref()
            .map_or_else(ProgressBar::hidden, |multi| {
                let bar = multi.add(ProgressBar::no_length());
                bar.set_style(spinner_bytes_style());
                bar.set_message(name.to_owned());
                bar.enable_steady_tick(Duration::from_millis(120));
                bar
            })
    }
}

/// Bar style for downloads with a known total size (MB transferred, speed).
#[must_use]
pub fn byte_bar_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{msg:<28!} [{bar:25}] {bytes:>10} / {total_bytes:<10} {bytes_per_sec}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
}

/// Spinner style for downloads whose total size is unknown.
fn spinner_bytes_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner} {msg:<28!} {bytes:>10} {bytes_per_sec}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

/// Prints rows as a space-aligned table on stdout (the table *is* command
/// data).
pub fn print_table(header: &[&str], rows: &[Vec<String>]) {
    for line in render_table(header, rows) {
        println!("{line}");
    }
}

/// Renders rows as space-aligned table lines. The last column is left
/// unpadded so long text never produces trailing whitespace.
fn render_table(header: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let columns = header.len();
    let mut widths: Vec<usize> = header.iter().map(|cell| cell.chars().count()).collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row.iter()).take(columns - 1) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let render = |cells: Vec<&str>| {
        let mut line = String::new();
        for (position, (cell, width)) in cells.iter().zip(&widths).enumerate() {
            if position + 1 == columns {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.extend(std::iter::repeat_n(' ', width - cell.chars().count() + 2));
            }
        }
        line.trim_end().to_owned()
    };

    let mut lines = vec![render(header.to_vec())];
    for row in rows {
        lines.push(render(row.iter().map(String::as_str).collect()));
    }
    lines
}

/// Formats a duration as a compact human-readable age such as `45s`, `12m`,
/// `3h` or `2d`.
#[must_use]
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
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

    /// Ages are formatted in the largest sensible unit.
    #[test]
    fn ages_use_compact_units() {
        assert_eq!(format_age(Duration::from_secs(45)), "45s");
        assert_eq!(format_age(Duration::from_secs(90)), "1m");
        assert_eq!(format_age(Duration::from_hours(2)), "2h");
        assert_eq!(format_age(Duration::from_secs(200_000)), "2d");
    }
}

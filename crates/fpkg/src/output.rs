//! Output: colours, messages on stderr, JSON on stdout.

use std::io::IsTerminal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Default)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug)]
pub struct Out {
    pub color: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub json: bool,
}

pub enum Style {
    Red,
    Yellow,
    Green,
    Dim,
}

impl Out {
    pub fn new(choice: ColorChoice, quiet: bool, verbose: bool, json: bool) -> Out {
        let color = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal(),
        };
        Out { color, quiet, verbose, json }
    }

    pub fn paint(&self, style: Style, s: &str) -> String {
        if !self.color {
            return s.to_string();
        }
        let code = match style {
            Style::Red => "31",
            Style::Yellow => "33",
            Style::Green => "32",
            Style::Dim => "2",
        };
        format!("\x1b[{code}m{s}\x1b[0m")
    }

    /// Progress and notes, on stderr; silenced by `-q`.
    pub fn status(&self, verb: &str, rest: &str) {
        if !self.quiet {
            eprintln!("{:>12} {rest}", self.paint(Style::Green, verb));
        }
    }

    pub fn note(&self, s: &str) {
        if !self.quiet {
            eprintln!("{s}");
        }
    }

    pub fn warn(&self, s: &str) {
        if !self.quiet {
            eprintln!("{} {s}", self.paint(Style::Yellow, "warning:"));
        }
    }

    pub fn error(&self, s: &str) {
        eprintln!("{} {s}", self.paint(Style::Red, "error:"));
    }

    pub fn verbose(&self, s: &str) {
        if self.verbose {
            eprintln!("{}", self.paint(Style::Dim, s));
        }
    }

    pub fn json(&self, v: &serde_json::Value) {
        println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
    }
}

/// Left-aligned columns.
pub fn table(rows: &[Vec<String>]) -> String {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; n];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(c);
            } else {
                line.push_str(&format!("{c:<w$}  ", w = widths[i]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

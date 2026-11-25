//! Output: colors, messages on stderr, JSON on stdout.

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
    /// Color on stderr, where messages go.
    pub color: bool,
    /// Color on stdout, where tables and `check` results go.
    pub color_stdout: bool,
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

fn paint(color: bool, style: Style, s: &str) -> String {
    if !color {
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

impl Out {
    pub fn new(choice: ColorChoice, quiet: bool, verbose: bool, json: bool) -> Out {
        let auto = |terminal: bool| match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => std::env::var_os("NO_COLOR").is_none() && terminal,
        };
        Out {
            color: auto(std::io::stderr().is_terminal()),
            color_stdout: auto(std::io::stdout().is_terminal()),
            quiet,
            verbose,
            json,
        }
    }

    /// `s` in `style`, for stderr.
    pub fn paint(&self, style: Style, s: &str) -> String {
        paint(self.color, style, s)
    }

    /// `s` in `style`, for stdout.
    pub fn paint_stdout(&self, style: Style, s: &str) -> String {
        paint(self.color_stdout, style, s)
    }

    /// A status line: the verb right-aligned in 12 columns and then painted,
    /// so that the escape codes take no width.
    fn status_line(&self, verb: &str, rest: &str) -> String {
        format!(
            "{} {rest}",
            self.paint(Style::Green, &format!("{verb:>12}"))
        )
    }

    /// Progress and notes, on stderr; silenced by `-q`.
    pub fn status(&self, verb: &str, rest: &str) {
        if !self.quiet {
            eprintln!("{}", self.status_line(verb, rest));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_verbs_are_aligned_with_and_without_color() {
        let mut out = Out::new(ColorChoice::Never, false, false, false);
        assert_eq!(
            out.status_line("Locking", "x 1.0.0"),
            "     Locking x 1.0.0"
        );
        out.color = true;
        assert_eq!(
            out.status_line("Locking", "x 1.0.0"),
            "\x1b[32m     Locking\x1b[0m x 1.0.0"
        );
        let always = Out::new(ColorChoice::Always, false, false, false);
        assert!(always.color && always.color_stdout);
    }
}

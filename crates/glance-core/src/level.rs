//! Level normalization: every Format's severity spelling maps onto one of six levels.

use std::fmt;

/// Severity of an Entry, ordered from least to most severe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Level; 6] = [
        Level::Trace,
        Level::Debug,
        Level::Info,
        Level::Warn,
        Level::Error,
        Level::Fatal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Fatal => "FATAL",
        }
    }

    /// Parses a severity as written in a log: a name, an alias or a number.
    ///
    /// Matching ignores case and surrounding whitespace or brackets. Returns
    /// `None` for anything unrecognized, meaning the Entry has no Level.
    pub fn parse(raw: &str) -> Option<Level> {
        let token = raw.trim().trim_matches(|c| matches!(c, '[' | ']' | '<' | '>' | ':'));
        if token.is_empty() {
            return None;
        }
        if let Ok(n) = token.parse::<i64>() {
            return Level::from_number(n);
        }
        let lowered = token.to_ascii_lowercase();
        Some(match lowered.as_str() {
            "trace" | "trc" | "t" | "finest" | "finer" | "verbose" | "vrb" => Level::Trace,
            "debug" | "dbg" | "d" | "fine" => Level::Debug,
            "info" | "inf" | "i" | "information" | "informational" | "notice" | "config" => {
                Level::Info
            }
            "warn" | "wrn" | "w" | "warning" => Level::Warn,
            "error" | "err" | "e" | "severe" => Level::Error,
            "fatal" | "f" | "crit" | "critical" | "alert" | "emerg" | "emergency" | "panic" => {
                Level::Fatal
            }
            _ => return None,
        })
    }

    /// Maps a numeric severity.
    ///
    /// Two conventions are accepted because their ranges do not overlap: syslog
    /// severities `0..=7` (0 is the most severe) and the Bunyan/Pino scale
    /// `10, 20, ..., 60` (trace to fatal).
    pub fn from_number(n: i64) -> Option<Level> {
        match n {
            0..=2 => Some(Level::Fatal),
            3 => Some(Level::Error),
            4 => Some(Level::Warn),
            5 | 6 => Some(Level::Info),
            7 => Some(Level::Debug),
            10 => Some(Level::Trace),
            20 => Some(Level::Debug),
            30 => Some(Level::Info),
            40 => Some(Level::Warn),
            50 => Some(Level::Error),
            60 => Some(Level::Fatal),
            _ => None,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_names_round_trip() {
        for level in Level::ALL {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
    }

    #[test]
    fn matching_ignores_case_and_decoration() {
        assert_eq!(Level::parse("error"), Some(Level::Error));
        assert_eq!(Level::parse("Error"), Some(Level::Error));
        assert_eq!(Level::parse("  [WARN]  "), Some(Level::Warn));
        assert_eq!(Level::parse("INFO:"), Some(Level::Info));
    }

    #[test]
    fn aliases_map_to_standard_levels() {
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert_eq!(Level::parse("err"), Some(Level::Error));
        assert_eq!(Level::parse("critical"), Some(Level::Fatal));
        assert_eq!(Level::parse("E"), Some(Level::Error));
        assert_eq!(Level::parse("notice"), Some(Level::Info));
        assert_eq!(Level::parse("verbose"), Some(Level::Trace));
    }

    #[test]
    fn numeric_syslog_and_pino_scales() {
        assert_eq!(Level::parse("3"), Some(Level::Error));
        assert_eq!(Level::parse("0"), Some(Level::Fatal));
        assert_eq!(Level::parse("30"), Some(Level::Info));
        assert_eq!(Level::parse("50"), Some(Level::Error));
        assert_eq!(Level::from_number(60), Some(Level::Fatal));
    }

    #[test]
    fn unknown_values_have_no_level() {
        for raw in ["", "   ", "[]", "banana", "8", "35", "-1", "errors"] {
            assert_eq!(Level::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn levels_order_by_severity() {
        assert!(Level::Warn < Level::Error);
        assert!(Level::Fatal > Level::Info);
        assert_eq!(Level::ALL.iter().max(), Some(&Level::Fatal));
    }
}

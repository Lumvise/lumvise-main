//! Bounded diagnostics captured from a child process that failed.
//!
//! Owns the single answer to "what did the process actually report" for
//! [`crate::error::NeuralError::ProcessFailed`]. Both streams are captured and
//! each is bounded here, so no constructor has to repeat truncation logic.
//!
//! Added for issue #88: a spawned Assistant Engine reported
//! `Not logged in - Please run /login` as JSON on **stdout** and wrote nothing
//! to stderr, so the recorded failure read `stderr: ` with an empty reason and
//! sent debugging in the wrong direction.
//!
//! # Example
//!
//! ```
//! use lumvise_neural_core::process::ProcessFailureDiagnostics;
//!
//! let diagnostics = ProcessFailureDiagnostics::from_streams(b"Not logged in", b"");
//! assert_eq!(diagnostics.to_string(), "stdout: Not logged in");
//! ```

use std::fmt;

/// Per-stream capture ceiling. A failure reason is read by a human or written to
/// a single log line, so each stream keeps at most this many bytes.
const MAX_STREAM_BYTES: usize = 2_048;

/// What a failed process wrote, bounded and ready to render as a reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessFailureDiagnostics {
    stdout: String,
    stderr: String,
}

impl ProcessFailureDiagnostics {
    /// Captures both streams of a process that has already exited.
    pub fn from_streams(stdout: &[u8], stderr: &[u8]) -> Self {
        Self {
            stdout: bounded_tail(stdout),
            stderr: bounded_tail(stderr),
        }
    }

    /// Captures a failure where only diagnostics text is available, such as a
    /// stderr file drained after the child is gone.
    pub fn from_stderr(stderr: impl AsRef<str>) -> Self {
        Self {
            stdout: String::new(),
            stderr: bounded_tail(stderr.as_ref().as_bytes()),
        }
    }

    /// Captures a failure a process announced on its stdout stream, such as a
    /// CLI that reports errors as JSON on stdout (#88).
    pub fn from_stdout(stdout: impl AsRef<str>) -> Self {
        Self {
            stdout: bounded_tail(stdout.as_ref().as_bytes()),
            stderr: String::new(),
        }
    }

    /// Records that a process failed without reporting anything.
    pub fn silent() -> Self {
        Self::default()
    }

    /// Everything the process reported on stdout, bounded.
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    /// Everything the process reported on stderr, bounded.
    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    /// True when the process reported nothing an operator could act on.
    pub fn is_silent(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

impl fmt::Display for ProcessFailureDiagnostics {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => formatter.write_str("no output captured"),
            (true, false) => write!(formatter, "stderr: {}", self.stderr),
            (false, true) => write!(formatter, "stdout: {}", self.stdout),
            (false, false) => {
                write!(
                    formatter,
                    "stdout: {}; stderr: {}",
                    self.stdout, self.stderr
                )
            }
        }
    }
}

/// Keeps the trailing bytes of a stream: a failing process reports its reason
/// last, so the tail is the actionable part. Marks a truncated capture with its
/// original size so an operator knows output was dropped.
fn bounded_tail(stream: &[u8]) -> String {
    let trimmed = String::from_utf8_lossy(stream).trim().to_string();
    if trimmed.len() <= MAX_STREAM_BYTES {
        return trimmed;
    }
    // Smallest char boundary whose tail fits the bound: char_indices ascends, so
    // the first match is the earliest byte we can keep without splitting a char.
    let tail_start = trimmed
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| trimmed.len() - index <= MAX_STREAM_BYTES)
        .unwrap_or(trimmed.len());
    format!(
        "[{} bytes captured, showing last {}] {}",
        trimmed.len(),
        trimmed.len() - tail_start,
        &trimmed[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_stdout_only_reason() {
        let diagnostics = ProcessFailureDiagnostics::from_streams(b"Not logged in", b"");

        assert_eq!(diagnostics.to_string(), "stdout: Not logged in");
        assert!(!diagnostics.is_silent());
    }

    #[test]
    fn renders_both_streams_when_both_reported() {
        let diagnostics = ProcessFailureDiagnostics::from_streams(b"out", b"err");

        assert_eq!(diagnostics.to_string(), "stdout: out; stderr: err");
    }

    #[test]
    fn names_a_process_that_reported_nothing() {
        let diagnostics = ProcessFailureDiagnostics::silent();

        assert_eq!(diagnostics.to_string(), "no output captured");
        assert!(diagnostics.is_silent());
    }

    #[test]
    fn bounds_each_stream_and_reports_the_captured_size() {
        let chatty = "x".repeat(200_000).into_bytes();

        let diagnostics = ProcessFailureDiagnostics::from_streams(&chatty, &chatty);

        assert!(diagnostics.stdout().len() < MAX_STREAM_BYTES * 2);
        assert!(diagnostics.stdout().contains("200000 bytes captured"));
        assert!(diagnostics.stderr().contains("200000 bytes captured"));
    }

    #[test]
    fn keeps_the_trailing_reason_of_a_bounded_stream() {
        let mut chatty = "noise\n".repeat(2_000);
        chatty.push_str("the actual reason");

        let diagnostics = ProcessFailureDiagnostics::from_streams(chatty.as_bytes(), b"");

        assert!(diagnostics.stdout().ends_with("the actual reason"));
    }

    #[test]
    fn bounds_a_stream_split_mid_character_without_panicking() {
        let mut chatty = "\u{4e16}".repeat(2_000);
        chatty.push_str("end");

        let diagnostics = ProcessFailureDiagnostics::from_streams(chatty.as_bytes(), b"");

        assert!(diagnostics.stdout().ends_with("end"));
    }
}

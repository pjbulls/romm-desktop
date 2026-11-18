//! A small log of what the UI is doing, for finding out why a window went dark.
//!
//! When a web view dies or stops painting there is nothing on screen and nothing in any console.
//! The page therefore reports its own state every couple of seconds (memory, size of the page,
//! how far it is scrolled, how late its timers run), and the backend writes those reports here,
//! with its own notes (the library loading) in between. After a failure the last lines say what
//! the page was doing, and whether it was still reporting after the screen went dark, which is
//! the difference between a web view that stopped painting and one that crashed or hung.
//!
//! It only ever holds counts and timings, never names or addresses, and it is capped.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How much one log file may hold before it is moved aside; with the file it replaces, about
/// two hours of two-second reports.
pub const MAX_LOG_BYTES: u64 = 256 * 1024;
/// Longest line kept; the page's own text (an error message) can be arbitrarily long.
const MAX_LINE_CHARS: usize = 600;

/// Local time with milliseconds, e.g. `2026-10-19 14:03:22.114`: what you'd read off a clock when the failure happened.
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// `<data dir>/diagnostics/ui.log`, and `ui.log.1` for the previous one.
pub struct LogFile {
    path: PathBuf,
    max_bytes: u64,
}

impl LogFile {
    pub fn new(data_dir: &Path) -> Self {
        LogFile { path: data_dir.join("diagnostics").join("ui.log"), max_bytes: MAX_LOG_BYTES }
    }

    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds one line. Best effort by design: a diagnostic must never get in the app's way, so a
    /// full disk or a locked file is ignored.
    pub fn append(&self, line: &str) {
        let _ = self.try_append(line);
    }

    fn try_append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        if fs::metadata(&self.path).map(|m| m.len() >= self.max_bytes).unwrap_or(false) {
            let old = self.path.with_extension("log.1");
            let _ = fs::remove_file(&old); // renaming over an existing file fails on Windows
            fs::rename(&self.path, &old)?;
        }
        let one_line: String = line.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).take(MAX_LINE_CHARS).collect();
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(format!("{one_line}\n").as_bytes())
    }
}

/// Notices when the page stops reporting while it is on screen.
///
/// Times are milliseconds on one clock the caller chooses. A page that isn't visible is throttled
/// or suspended by the browser, so silence from it proves nothing and is not reported.
pub struct Watchdog {
    silent_after_ms: u64,
    /// The time of the last report before the silence began, while one is going on.
    silent_since: Option<u64>,
}

impl Watchdog {
    pub fn new(silent_after: Duration) -> Self {
        Watchdog { silent_after_ms: silent_after.as_millis() as u64, silent_since: None }
    }

    /// Call periodically. Returns a line to log when the page has just gone quiet or has just
    /// started reporting again; each happens once per outage.
    pub fn check(&mut self, now_ms: u64, last_report_ms: Option<u64>, page_visible: bool) -> Option<String> {
        let last = last_report_ms?; // the page hasn't started reporting yet
        let quiet_for = now_ms.saturating_sub(last);
        match self.silent_since {
            None if quiet_for >= self.silent_after_ms && page_visible => {
                self.silent_since = Some(last);
                Some(format!("WATCHDOG the page has not reported for {}s while visible: it is hung, or its process has died", quiet_for / 1000))
            }
            Some(since) if quiet_for < self.silent_after_ms => {
                self.silent_since = None;
                Some(format!("WATCHDOG the page is reporting again after {}s of silence", last.saturating_sub(since) / 1000))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_in(dir: &Path, max: u64) -> LogFile {
        LogFile::new(dir).with_max_bytes(max)
    }

    #[test]
    fn appends_lines_and_creates_its_folder() {
        let d = tempfile::tempdir().unwrap();
        let l = log_in(d.path(), MAX_LOG_BYTES);
        l.append("one");
        l.append("two");
        assert_eq!(fs::read_to_string(l.path()).unwrap(), "one\ntwo\n");
        assert!(l.path().ends_with("diagnostics/ui.log") || l.path().ends_with("diagnostics\\ui.log"));
    }

    #[test]
    fn a_line_stays_one_line_and_is_capped() {
        let d = tempfile::tempdir().unwrap();
        let l = log_in(d.path(), MAX_LOG_BYTES);
        l.append("a\nb\r\nc");
        l.append(&"x".repeat(5000));
        let text = fs::read_to_string(l.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "a b  c");
        assert_eq!(lines[1].chars().count(), MAX_LINE_CHARS);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn a_full_file_is_moved_aside_and_the_one_before_it_dropped() {
        let d = tempfile::tempdir().unwrap();
        let l = log_in(d.path(), 20);
        l.append("first line is long enough"); // over the cap
        l.append("second");
        let old = d.path().join("diagnostics").join("ui.log.1");
        assert_eq!(fs::read_to_string(&old).unwrap(), "first line is long enough\n");
        assert_eq!(fs::read_to_string(l.path()).unwrap(), "second\n");
        // Rotating again replaces the old copy rather than failing (a rename over a file fails on Windows).
        l.append("third line is also long enough");
        l.append("fourth");
        assert_eq!(fs::read_to_string(&old).unwrap(), "second\nthird line is also long enough\n");
        assert_eq!(fs::read_to_string(l.path()).unwrap(), "fourth\n");
    }

    #[test]
    fn an_unwritable_place_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        let blocker = d.path().join("diagnostics");
        fs::write(&blocker, "a file where the folder should be").unwrap();
        log_in(d.path(), MAX_LOG_BYTES).append("goes nowhere, and must not panic");
    }

    #[test]
    fn the_watchdog_is_quiet_until_the_page_has_reported() {
        let mut w = Watchdog::new(Duration::from_secs(10));
        assert_eq!(w.check(60_000, None, true), None);
    }

    #[test]
    fn the_watchdog_is_quiet_while_the_page_keeps_reporting() {
        let mut w = Watchdog::new(Duration::from_secs(10));
        assert_eq!(w.check(5_000, Some(4_000), true), None);
        assert_eq!(w.check(11_000, Some(9_000), true), None);
    }

    #[test]
    fn the_watchdog_reports_a_silence_once_and_then_the_recovery_once() {
        let mut w = Watchdog::new(Duration::from_secs(10));
        let gone = w.check(20_000, Some(5_000), true).expect("15s of silence while visible");
        assert!(gone.contains("15s") && gone.contains("hung"), "{gone}");
        assert_eq!(w.check(25_000, Some(5_000), true), None, "not again while it goes on");
        assert_eq!(w.check(40_000, Some(5_000), true), None);
        let back = w.check(52_000, Some(51_000), true).expect("a report arrived");
        assert!(back.contains("46s"), "{back}"); // silent from its last report at 5s to its next at 51s
        assert_eq!(w.check(55_000, Some(54_000), true), None);
        // A later outage is reported afresh.
        assert!(w.check(80_000, Some(54_000), true).is_some());
    }

    #[test]
    fn a_hidden_page_is_not_accused_of_anything() {
        let mut w = Watchdog::new(Duration::from_secs(10));
        assert_eq!(w.check(120_000, Some(1_000), false), None);
        // ...but once it is visible and still silent, it is.
        assert!(w.check(121_000, Some(1_000), true).is_some());
    }
}

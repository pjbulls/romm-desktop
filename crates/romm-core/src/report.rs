//! What one kind of sync (saves, or save states) did for a game, and the total over several games.
//!
//! Saves and states are synced by different engines but report in the same terms, so there is one
//! report for both. `SyncReport` and `StateReport` are the same type under the name that says what
//! it is about.

use serde::Serialize;

/// A file both sides changed that was not settled by itself: the conflict policy is `Ask`, or the
/// server refused to overwrite what another device put there.
#[derive(Debug, Clone, Serialize)]
pub struct PendingConflict {
    pub rom_id: i64,
    pub file_name: String,
    pub server_updated_at: Option<String>,
    /// What the server said about it, when it said anything.
    pub reason: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct TransferReport {
    pub uploaded: Vec<String>,
    pub downloaded: Vec<String>,
    /// Local copies preserved on the server as `<name>.conflict-<time>` while a conflict was settled.
    pub archived: Vec<String>,
    pub conflicts: Vec<PendingConflict>,
    /// Too large for the size limit, so neither uploaded nor downloaded (save states only).
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
    pub unchanged: usize,
}

impl TransferReport {
    /// Nothing failed and nothing is waiting for the user to decide.
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty() && self.conflicts.is_empty()
    }

    /// Nothing happened, bar files found unchanged.
    pub fn is_empty(&self) -> bool {
        let TransferReport { uploaded, downloaded, archived, conflicts, skipped, failed, unchanged: _ } = self;
        uploaded.is_empty() && downloaded.is_empty() && archived.is_empty() && conflicts.is_empty() && skipped.is_empty() && failed.is_empty()
    }

    /// The files in conflict, by name.
    pub fn conflict_names(&self) -> Vec<&str> {
        self.conflicts.iter().map(|c| c.file_name.as_str()).collect()
    }

    /// Adds `other` to this report, to total up several games. Every field is named on purpose: a
    /// field added to the report is a compile error here until it is counted, so it can't be left
    /// out of a total.
    pub fn merge(&mut self, other: TransferReport) {
        let TransferReport { uploaded, downloaded, archived, conflicts, skipped, failed, unchanged } = other;
        self.uploaded.extend(uploaded);
        self.downloaded.extend(downloaded);
        self.archived.extend(archived);
        self.conflicts.extend(conflicts);
        self.skipped.extend(skipped);
        self.failed.extend(failed);
        self.unchanged += unchanged;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(n: &str) -> PendingConflict {
        PendingConflict { rom_id: 1, file_name: n.into(), server_updated_at: None, reason: None }
    }

    #[test]
    fn merging_reports_adds_every_outcome_and_keeps_their_order() {
        let one = |s: &str| vec![s.to_string()];
        let mut total = TransferReport {
            uploaded: one("u"),
            downloaded: one("d"),
            archived: one("a"),
            conflicts: vec![conflict("c")],
            skipped: one("s"),
            failed: one("f"),
            unchanged: 1,
        };
        total.merge(TransferReport {
            uploaded: one("u2"),
            downloaded: one("d2"),
            archived: one("a2"),
            conflicts: vec![conflict("c2")],
            skipped: one("s2"),
            failed: one("f2"),
            unchanged: 4,
        });
        assert_eq!(total.uploaded, ["u", "u2"]);
        assert_eq!(total.downloaded, ["d", "d2"]);
        // A total over many games must still say that a local copy was archived, or skipped for its size.
        assert_eq!(total.archived, ["a", "a2"]);
        assert_eq!(total.conflict_names(), ["c", "c2"]);
        assert_eq!(total.skipped, ["s", "s2"]);
        assert_eq!(total.failed, ["f", "f2"]);
        assert_eq!(total.unchanged, 5);
    }

    #[test]
    fn a_report_is_clean_unless_something_failed_or_awaits_a_decision() {
        assert!(TransferReport::default().is_clean());
        assert!(TransferReport { uploaded: vec!["x".into()], skipped: vec!["y".into()], ..Default::default() }.is_clean());
        assert!(!TransferReport { failed: vec!["x".into()], ..Default::default() }.is_clean());
        assert!(!TransferReport { conflicts: vec![conflict("x")], ..Default::default() }.is_clean());
    }

    #[test]
    fn a_report_of_files_found_unchanged_is_empty() {
        assert!(TransferReport { unchanged: 3, ..Default::default() }.is_empty());
        assert!(!TransferReport { archived: vec!["x".into()], ..Default::default() }.is_empty());
    }
}

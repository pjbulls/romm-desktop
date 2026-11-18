//! Files that two programs write: `state.json` and `config.toml` are shared by the desktop app and
//! the CLI, and each of them used to read the file once, keep it in memory for as long as it ran
//! (the CLI's `play` for a whole emulator session) and write the whole of it back. Whatever the
//! other program had saved in between was overwritten, and the two even wrote through one fixed
//! temp name.
//!
//! The fix is in two parts. A program remembers what the file held when it last read or wrote it
//! (its *base*), and saving becomes a short critical section under `FileLock`: read what is on disk
//! now, `merge3` it with this program's copy against that base, write the result, and adopt it, so
//! the copy in memory also learns what the other program did.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

/// The longest a save waits for the other program to finish its own. Holding the lock takes
/// milliseconds (read, merge, write), so this is only reached if the other program is stuck, and
/// then the save goes ahead without the lock, as it always did, rather than not saving.
pub const LOCK_WAIT: Duration = Duration::from_secs(5);

/// An exclusive lock on `<target>.lock`, held until it is dropped. The operating system releases it
/// if the program dies, so it cannot be left behind.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Waits up to `wait` for the lock on `target`'s companion file. `None` if it could not be had
    /// (the other program kept it, or the file cannot be locked here): the caller carries on without.
    pub fn acquire(target: &Path, wait: Duration) -> Option<FileLock> {
        let mut name = target.file_name()?.to_os_string();
        name.push(".lock");
        let path = target.with_file_name(name);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path).ok()?;
        let give_up = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Some(FileLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < give_up => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return None,
            }
        }
    }
}

/// Merges two edits of one JSON document, `mine` and `theirs`, that were both made from `base`.
///
/// The unit of change is a *record*: a top-level field, or one entry of a top-level table (a game's
/// install, a save's last-synced hash). For each record, if this side did not change it the other
/// side's version is taken (a deletion included); if it did, this side's is. Where both changed the
/// same record, this side wins, as the later writer always did, but a record is never made of
/// pieces of both: its fields describe one exchange with the server and mean nothing apart.
///
/// If either document is not an object (a damaged file) there is nothing to merge and `mine` stands.
pub fn merge3(base: &Value, mine: &Value, theirs: &Value) -> Value {
    let (Some(m), Some(t)) = (mine.as_object(), theirs.as_object()) else { return mine.clone() };
    let empty = Map::new();
    let b = base.as_object().unwrap_or(&empty);
    let mut out = Map::new();
    for key in keys(&[b, m, t]) {
        let (bv, mv, tv) = (b.get(key), m.get(key), t.get(key));
        let merged = match (mv, tv) {
            // A table of records: settle each entry by itself.
            (Some(Value::Object(mt)), Some(Value::Object(tt))) => {
                let bt = bv.and_then(Value::as_object).unwrap_or(&empty);
                let mut table = Map::new();
                for entry in keys(&[bt, mt, tt]) {
                    if let Some(v) = pick(bt.get(entry), mt.get(entry), tt.get(entry)) {
                        table.insert(entry.clone(), v.clone());
                    }
                }
                Some(Value::Object(table))
            }
            _ => pick(bv, mv, tv).cloned(),
        };
        if let Some(v) = merged {
            out.insert(key.clone(), v);
        }
    }
    Value::Object(out)
}

/// What one record becomes: the other side's unless this side changed it. `None` is "absent".
fn pick<'a>(base: Option<&'a Value>, mine: Option<&'a Value>, theirs: Option<&'a Value>) -> Option<&'a Value> {
    if mine == base {
        theirs
    } else {
        mine
    }
}

fn keys<'a>(maps: &[&'a Map<String, Value>]) -> BTreeSet<&'a String> {
    maps.iter().flat_map(|m| m.keys()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_the_other_program_changed_and_this_one_did_not_is_kept() {
        let base = json!({"roms": {"1": "a"}, "saves": {"1:x": {"h": "1"}}});
        let mine = base.clone();
        let theirs = json!({"roms": {"1": "a", "2": "b"}, "saves": {"1:x": {"h": "2"}}});
        assert_eq!(merge3(&base, &mine, &theirs), theirs);
    }

    #[test]
    fn what_this_program_changed_and_the_other_did_not_is_kept() {
        let base = json!({"roms": {"1": "a"}});
        let mine = json!({"roms": {"1": "a", "2": "b"}});
        assert_eq!(merge3(&base, &mine, &base), mine);
    }

    #[test]
    fn changes_to_different_records_of_one_table_both_survive() {
        let base = json!({"roms": {"1": "a"}, "saves": {"1:x": {"h": "1"}, "2:x": {"h": "1"}}});
        let mine = json!({"roms": {"1": "a"}, "saves": {"1:x": {"h": "mine"}, "2:x": {"h": "1"}}});
        let theirs = json!({"roms": {"1": "a", "9": "new"}, "saves": {"1:x": {"h": "1"}, "2:x": {"h": "theirs"}}});
        let merged = merge3(&base, &mine, &theirs);
        assert_eq!(merged["saves"]["1:x"]["h"], "mine");
        assert_eq!(merged["saves"]["2:x"]["h"], "theirs");
        assert_eq!(merged["roms"]["9"], "new", "an install made by the other program is not lost");
    }

    #[test]
    fn a_record_both_changed_is_this_programs_whole_and_never_a_mix() {
        let base = json!({"saves": {"1:x": {"hash": "0", "at": "0"}}});
        let mine = json!({"saves": {"1:x": {"hash": "mine", "at": "0"}}});
        let theirs = json!({"saves": {"1:x": {"hash": "0", "at": "theirs"}}});
        assert_eq!(merge3(&base, &mine, &theirs)["saves"]["1:x"], json!({"hash": "mine", "at": "0"}));
    }

    #[test]
    fn a_deletion_by_either_program_holds_unless_the_other_changed_that_record() {
        let base = json!({"roms": {"1": "a", "2": "b", "3": "c"}});
        // This program uninstalled 1 and 3; the other changed 3 and left 1 and 2 alone.
        let mine = json!({"roms": {"2": "b"}});
        let theirs = json!({"roms": {"1": "a", "2": "b", "3": "changed"}});
        let merged = merge3(&base, &mine, &theirs);
        assert_eq!(merged["roms"], json!({"2": "b"}), "a deletion is this program's change like any other");
        // The other way round: the other program uninstalled 2, this one did nothing to it.
        let mine = base.clone();
        let theirs = json!({"roms": {"1": "a", "3": "c"}});
        assert_eq!(merge3(&base, &mine, &theirs)["roms"], json!({"1": "a", "3": "c"}));
    }

    #[test]
    fn scalar_fields_and_a_missing_base_are_handled() {
        let mine = json!({"server_url": "http://a", "device_id": null});
        let theirs = json!({"server_url": "http://b", "device_id": "dev"});
        // No base (a first save): nothing is known to be unchanged, so this side's values stand where
        // they differ and the other side's fields that this side lacks are kept.
        let merged = merge3(&Value::Null, &mine, &theirs);
        assert_eq!(merged["server_url"], "http://a");
        let merged = merge3(&json!({"server_url": "http://b"}), &json!({"server_url": "http://b"}), &theirs);
        assert_eq!(merged["server_url"], "http://b");
        assert_eq!(merged["device_id"], "dev");
    }

    #[test]
    fn a_table_that_is_new_to_one_side_is_merged_as_a_whole() {
        let merged = merge3(&json!({}), &json!({"saves": {"1:x": 1}}), &json!({"saves": {"2:x": 2}}));
        assert_eq!(merged["saves"], json!({"1:x": 1, "2:x": 2}));
    }

    #[test]
    fn a_damaged_document_leaves_this_programs_copy_as_it_is() {
        let mine = json!({"a": 1});
        assert_eq!(merge3(&json!({}), &mine, &json!("garbage")), mine);
        assert_eq!(merge3(&json!({}), &json!([1]), &json!({"a": 1})), json!([1]));
    }

    #[test]
    fn the_lock_is_exclusive_and_is_released_on_drop() {
        let t = tempfile::tempdir().unwrap();
        let target = t.path().join("sub/state.json");
        let first = FileLock::acquire(&target, Duration::from_secs(1)).expect("the first lock is free");
        let started = Instant::now();
        assert!(FileLock::acquire(&target, Duration::from_millis(60)).is_none(), "held: the second waits and gives up");
        assert!(started.elapsed() >= Duration::from_millis(50));
        drop(first);
        assert!(FileLock::acquire(&target, Duration::from_millis(60)).is_some(), "free again once dropped");
    }
}

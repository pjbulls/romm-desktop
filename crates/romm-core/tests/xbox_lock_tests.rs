//! Keeping xemu and this crate off the same disk image. The first group runs on every platform
//! (Windows and Linux alike); the second needs a real QEMU process and runs on Linux, skipping where
//! `qemu-io` is not installed.

mod common;

use std::path::{Path, PathBuf};

use romm_core::xbox::lock::open_exclusive;
use romm_core::xbox::qcow2::{journal_path, recover_journal, Qcow2};
use romm_core::xbox::{check_writable, image_in_use, import_save, BlockWrite, FileHdd};
use zip::write::SimpleFileOptions;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/xbox").join(name)
}

fn copy_fixture(dir: &Path, name: &str) -> PathBuf {
    let img = dir.join(name);
    std::fs::copy(fixture(name), &img).unwrap();
    img
}

fn tiny_save(id: &str) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    z.start_file(format!("{id}/a.bin"), SimpleFileOptions::default()).unwrap();
    z.write_all(&[7u8; 5000]).unwrap();
    z.finish().unwrap().into_inner()
}

// ---- every platform ---------------------------------------------------------------------------

#[test]
fn a_held_image_is_seen_as_in_use_by_everyone_else_and_is_free_again_afterwards() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    assert!(image_in_use(&img).is_none());
    check_writable(&img).unwrap();

    let held = open_exclusive(&img).unwrap();
    assert!(image_in_use(&img).is_some(), "someone writing is noticed");
    let e = open_exclusive(&img).unwrap_err().to_string();
    assert!(e.contains("open in another program"), "a second writer is refused: {e}");
    assert!(check_writable(&img).is_err());
    // Reading is still allowed (the sync engine decides for itself not to trust it).
    FileHdd::open(&img).expect("others can still read an image someone is writing");

    drop(held);
    assert!(image_in_use(&img).is_none(), "released when the holder lets go (or dies)");
    open_exclusive(&img).expect("and it can be taken again");
}

#[test]
fn a_restore_is_refused_while_the_image_is_held_and_changes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let original = std::fs::read(&img).unwrap();

    let held = open_exclusive(&img).unwrap(); // stands for xemu
    let e = import_save(&img, "4D530004", &tiny_save("4D530004")).unwrap_err().to_string();
    assert!(e.contains("open in another program"), "{e}");
    assert_eq!(std::fs::read(&img).unwrap(), original, "the image is untouched");
    assert!(!journal_path(&img).exists(), "and no journal was started");

    drop(held);
    import_save(&img, "4D530004", &tiny_save("4D530004")).expect("with xemu gone it goes through");
}

#[test]
fn an_interrupted_write_is_not_undone_underneath_a_program_that_has_the_image_open() {
    let t = tempfile::tempdir().unwrap();
    let img = copy_fixture(t.path(), "hdd-v3.qcow2");
    let original = std::fs::read(&img).unwrap();

    // A write that died half way (the process is gone; the journal is what is left).
    let mut disk = Qcow2::new(std::fs::OpenOptions::new().read(true).write(true).open(&img).unwrap()).unwrap();
    disk.begin(Some(&journal_path(&img))).unwrap();
    disk.write_at(E_START, &[9u8; 100_000]).unwrap();
    drop(disk);
    let damaged = std::fs::read(&img).unwrap();
    assert_ne!(damaged, original);

    // Somebody has the image open now: putting the old bytes back under it would corrupt it.
    let held = open_exclusive(&img).unwrap();
    let e = recover_journal(&img).unwrap_err().to_string();
    assert!(e.contains("in use"), "{e}");
    assert_eq!(std::fs::read(&img).unwrap(), damaged, "nothing was changed under the other program");
    drop(held);

    assert!(recover_journal(&img).unwrap());
    assert_eq!(std::fs::read(&img).unwrap(), original);
}

const E_START: u64 = romm_core::xbox::E_OFFSET;

// ---- a real QEMU (Linux) ----------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod with_qemu {
    use super::*;
    use common::QemuHolder;

    fn skip() {
        eprintln!("SKIPPED: qemu-io is not installed, so there is no real QEMU to hold an image open");
    }

    #[test]
    fn a_running_qemu_is_noticed_and_a_restore_waits_for_it_to_close() {
        let t = tempfile::tempdir().unwrap();
        let img = copy_fixture(t.path(), "hdd-v3.qcow2");
        let original = std::fs::read(&img).unwrap();
        let Some(qemu) = QemuHolder::start(&img) else { return skip() };

        // What xemu would be doing: the image is open read-write in QEMU.
        assert!(image_in_use(&img).is_some());
        assert!(check_writable(&img).is_err());
        let e = import_save(&img, "4D530004", &tiny_save("4D530004")).unwrap_err().to_string();
        assert!(e.contains("open in another program"), "{e}");
        assert_eq!(std::fs::read(&img).unwrap(), original, "QEMU's image is left alone");
        assert!(!journal_path(&img).exists());

        qemu.stop(&img);
        assert!(image_in_use(&img).is_none());
        import_save(&img, "4D530004", &tiny_save("4D530004")).expect("once xemu is closed it goes through");
    }

    #[test]
    fn a_read_only_qemu_does_not_get_in_the_way() {
        let t = tempfile::tempdir().unwrap();
        let img = copy_fixture(t.path(), "hdd-v3.qcow2");
        let Ok(mut child) = std::process::Command::new("qemu-io").args(["-r", "-c", "sleep 120000"]).arg(&img).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() else { return skip() };
        std::thread::sleep(std::time::Duration::from_millis(800));
        let free = image_in_use(&img).is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert!(free, "somebody only reading the image is not a reason to refuse");
    }

    #[test]
    fn while_a_restore_holds_the_image_qemu_itself_refuses_to_open_it() {
        if std::process::Command::new("qemu-io").arg("--version").output().is_err() {
            return skip();
        }
        let t = tempfile::tempdir().unwrap();
        let img = copy_fixture(t.path(), "hdd-v3.qcow2");
        let try_open = || std::process::Command::new("qemu-io").args(["-c", "read 0 512"]).arg(&img).output().unwrap();

        assert!(try_open().status.success(), "QEMU opens a free image");
        let held = open_exclusive(&img).unwrap();
        let out = try_open();
        let said = String::from_utf8_lossy(&out.stderr).to_lowercase() + &String::from_utf8_lossy(&out.stdout).to_lowercase();
        assert!(!out.status.success() && said.contains("lock"), "xemu must not be able to start on an image that is being written: {said}");
        drop(held);
        assert!(try_open().status.success(), "and it can again once the restore is done");
    }
}

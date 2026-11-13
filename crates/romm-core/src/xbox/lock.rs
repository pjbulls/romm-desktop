//! Keeping this crate and xemu off the same disk image at the same moment.
//!
//! xemu is QEMU, and QEMU guards a disk image it has open against other users, but each system does
//! it its own way, so this does too:
//!
//! * **Windows.** QEMU opens an image with a share mode that lets others read but not write it. So
//!   opening the image for writing fails with a sharing violation while xemu has it, which is how
//!   [`in_use`] notices, and an exclusive open here (readers allowed, writers not) makes xemu fail to
//!   open the image while a save is being put back.
//! * **Linux.** QEMU takes byte-range locks on the image file: bytes 100, 101, 103, 201 and 203
//!   when it has the image open read-write (measured against a real QEMU process; read-only opens
//!   take 100, 201 and 203). Byte 101 is its permission to write, so a conflicting lock there means
//!   someone can write. Taking the same locks makes QEMU itself refuse to open the image, which is
//!   what `qemu-img` does for a running VM. A "someone has it" answer is asked again for a moment
//!   before it is believed: a process being spawned briefly holds a copy of every open file.
//! * **Elsewhere** nothing is detected and nothing is held; the in-use flag in the image header is
//!   all that is left.
//!
//! None of this can help against a program that ignores it, only against QEMU and this crate.

use std::fs::{File, OpenOptions};
#[cfg(windows)]
use std::io;
use std::path::Path;

use super::{XboxError, XboxResult};

fn in_use_error(path: &Path, how: &str) -> XboxError {
    XboxError::Fatx(format!("the disk image {} is open in another program: {how}", path.display()))
}

#[cfg(target_os = "linux")]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    /// QEMU's permission-lock bytes start here, one per permission bit; "write" is bit 1.
    const WRITE_PERMISSION: i64 = 100 + 1;
    /// What QEMU holds on an image it has open read-write (measured, see the module docs).
    const HELD_BY_A_WRITER: [i64; 5] = [100, 101, 103, 201, 203];

    fn flock(kind: libc::c_int, byte: i64) -> libc::flock {
        // SAFETY: `flock` is plain old data; all-zero is a valid value and the used fields are set.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = kind as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = byte as _;
        fl.l_len = 1;
        fl.l_pid = 0; // open-file-description locks require 0
        fl
    }

    /// Whether another open file description holds QEMU's write permission on this file.
    pub fn another_writer(f: &File) -> io::Result<bool> {
        let mut fl = flock(libc::F_WRLCK, WRITE_PERMISSION);
        // SAFETY: valid fd and a valid, initialised `flock`.
        let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_OFD_GETLK, &mut fl) };
        if r == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(fl.l_type as libc::c_int != libc::F_UNLCK)
    }

    /// How many times, and how far apart, a "yes" is asked again before it is believed.
    const SETTLE_ASKS: u32 = 12;
    const SETTLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(10);

    /// [`another_writer`], but a "yes" is believed only after asking again for about a tenth of a second.
    ///
    /// These locks belong to an open file description, and a process being spawned (an emulator,
    /// `qemu-img`) holds a copy of every file this one has open from the fork until it execs. Spawning
    /// on one thread while another has just let go of the image therefore makes it look held, for a few
    /// milliseconds, by something that does not have it. A program that really has the image open (xemu)
    /// keeps its locks for as long as it runs, so asking again tells the two apart. "No" is answered at once.
    pub fn another_writer_settled(f: &File) -> io::Result<bool> {
        for ask in 1..=SETTLE_ASKS {
            if !another_writer(f)? {
                return Ok(false);
            }
            if ask < SETTLE_ASKS {
                std::thread::sleep(SETTLE_PAUSE);
            }
        }
        Ok(true)
    }

    /// Takes the locks a read-write QEMU takes, for as long as `f` stays open. `Ok(false)` if
    /// someone else's locks forbid it; `Ok(true)` if it is held, or if this filesystem has no such
    /// locks (some network shares), where QEMU does without them too.
    pub fn hold(f: &File) -> io::Result<bool> {
        for byte in HELD_BY_A_WRITER {
            let fl = flock(libc::F_RDLCK, byte);
            // SAFETY: valid fd and a valid `flock`.
            let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_OFD_SETLK, &fl) };
            if r == -1 {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::EAGAIN) | Some(libc::EACCES) => Ok(false),
                    _ => Ok(true),
                };
            }
        }
        Ok(true)
    }
}

/// Whether `e` is Windows saying another program has the file open in a way that forbids this.
#[cfg(windows)]
fn sharing_violation(e: &io::Error) -> bool {
    // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// Why the image looks to be in use by another program right now, or `None` if nothing says so.
/// Best effort, and changes nothing: it only asks.
pub fn in_use(path: &Path) -> Option<String> {
    in_use_here(path)
}

#[cfg(target_os = "linux")]
fn in_use_here(path: &Path) -> Option<String> {
    let f = File::open(path).ok()?;
    match sys::another_writer_settled(&f) {
        Ok(true) => Some("another program has it open".into()),
        _ => None,
    }
}

#[cfg(windows)]
fn in_use_here(path: &Path) -> Option<String> {
    // Asking for write access is refused while a program that shares the file only for reading, as
    // QEMU does, has it open. Nothing is written.
    match OpenOptions::new().read(true).write(true).open(path) {
        Err(e) if sharing_violation(&e) => Some("another program has it open".into()),
        _ => None,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn in_use_here(_path: &Path) -> Option<String> {
    None
}

/// Opens the image for reading and writing, and holds it so that QEMU (xemu) cannot open it for
/// as long as the returned file is kept. Fails if it is already open elsewhere.
pub fn open_exclusive(path: &Path) -> XboxResult<File> {
    open_exclusive_here(path)
}

#[cfg(windows)]
fn open_exclusive_here(path: &Path) -> XboxResult<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ only: others may read the image, none may write it.
    OpenOptions::new().read(true).write(true).share_mode(1).open(path).map_err(|e| {
        if sharing_violation(&e) {
            in_use_error(path, "close it and try again")
        } else {
            e.into()
        }
    })
}

#[cfg(target_os = "linux")]
fn open_exclusive_here(path: &Path) -> XboxResult<File> {
    let f = OpenOptions::new().read(true).write(true).open(path)?;
    // A filesystem that cannot answer is treated as having no writer, as QEMU would.
    if sys::another_writer_settled(&f).unwrap_or(false) || !sys::hold(&f).map_err(XboxError::Io)? {
        return Err(in_use_error(path, "close it and try again"));
    }
    Ok(f)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn open_exclusive_here(path: &Path) -> XboxResult<File> {
    Ok(OpenOptions::new().read(true).write(true).open(path)?)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn image() -> (tempfile::TempDir, std::path::PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("hdd.qcow2");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        (t, p)
    }

    /// A process being spawned holds a copy of every file descriptor this one has open, from the fork
    /// until it execs. An open-file-description lock is only released when the last copy is closed, so
    /// in that gap an image this thread has just let go of still looks held by someone else. Nothing
    /// else has it; asking again a moment later says so.
    ///
    /// The gap is made on purpose and long enough to be sure of: the child says it exists, then
    /// lingers 40 ms before it execs.
    #[test]
    fn an_image_just_let_go_of_is_not_in_use_because_a_child_is_still_forking() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;

        let (_dir, path) = image();
        let held = open_exclusive(&path).expect("nobody has it");

        let (mut told, tell) = std::io::pipe().unwrap();
        let tell_fd = tell.as_raw_fd();
        let spawner = std::thread::spawn(move || {
            let _keep_the_pipe_open = tell;
            let mut child = std::process::Command::new("true");
            // SAFETY: between fork and exec only `write` and `usleep` are called, both async-signal-safe.
            unsafe {
                child.pre_exec(move || {
                    libc::write(tell_fd, b"x".as_ptr().cast(), 1);
                    libc::usleep(40_000);
                    Ok(())
                });
            }
            child.status().unwrap();
        });

        told.read_exact(&mut [0u8]).unwrap(); // the child exists, has a copy of `held`, and has not exec'd
        drop(held); // this side has let go; the child's copy has not
        let next = open_exclusive(&path);
        spawner.join().unwrap();

        let next = next.map(drop);
        assert!(next.is_ok(), "an image nobody has open was refused because a child was mid-spawn: {next:?}");
        assert!(in_use(&path).is_none());
    }

    /// The other side: something that really has the image open is still noticed, and still refused,
    /// after being asked a few times rather than once.
    #[test]
    fn an_image_somebody_really_holds_is_still_in_use_and_still_refused() {
        let (_dir, path) = image();
        let holder = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        assert!(sys::hold(&holder).unwrap(), "the holder takes the locks a read-write QEMU takes");

        let started = Instant::now();
        assert!(in_use(&path).is_some(), "a real holder is noticed");
        let err = open_exclusive(&path).expect_err("a real holder is refused").to_string();
        assert!(err.contains("open in another program"), "{err}");
        // It asked more than once before believing that: a lower bound that does not depend on load.
        assert!(started.elapsed() >= Duration::from_millis(100), "answered at once: {:?}", started.elapsed());

        drop(holder);
        assert!(in_use(&path).is_none(), "and once it lets go, it is free again");
        drop(open_exclusive(&path).expect("free again"));
    }
}

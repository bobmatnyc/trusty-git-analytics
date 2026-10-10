//! The one writer for a stub executable a test runs (#152).
//!
//! Why: on Linux, `execve` fails with `ETXTBSY` while ANY open file
//! description holds the file's inode open for writing. A test that writes a
//! stub with `std::fs::write` holds such a descriptor in its own process for a
//! moment. A sibling test thread that forks in that moment copies the
//! descriptor into its child, and the copy lives until that child execs or
//! exits. Closing the descriptor in this thread does not close the copy, so
//! the stub's own exec can still fail. Writing to a temporary name and
//! renaming it does not help either: the rename keeps the inode, and the
//! leaked copy still points at it.
//!
//! What: [`write_stub_executable`] never opens the stub for writing in the
//! test process. A `cat` child process creates and writes the file from a
//! pipe, and the helper waits for that child to exit before it sets mode 0755.
//! The only write descriptor the inode ever has lives in that child's fd
//! table, and a fork copies the forking process's table, never the child's.
//! When the wait returns, the child has exited and the kernel has closed the
//! descriptor, so no process can hold the inode open for writing.
//!
//! Compiled into the library's unit tests through `lib.rs` and into each
//! integration test that runs a stub through `#[path]`, so every caller shares
//! this one file.
//!
//! Test: every stub-running test calls it, for example
//! `grounding::index::index_tests::a_refused_index_is_a_reason_not_a_status`.

use std::path::Path;

/// Write `script` to `path` and make it executable, so that running `path`
/// can never fail with `ETXTBSY`. Panics on any failure: this is test setup.
///
/// An existing file at `path` is removed first, so the stub is always a fresh
/// inode. Truncating the old inode in place would fail with `ETXTBSY` if a
/// process were still running it.
#[cfg(unix)]
pub fn write_stub_executable(path: &Path, script: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::{Command, Stdio};

    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("remove the old stub {}: {e}", path.display()),
    }
    // `exec` leaves `cat` as the only process that holds the write descriptor.
    let mut writer = Command::new("/bin/sh")
        .arg("-c")
        .arg("exec /bin/cat > \"$1\"")
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the stub writer");
    writer
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(script.as_bytes())
        .expect("send the stub to its writer");
    let output = writer.wait_with_output().expect("wait for the stub writer");
    assert!(
        output.status.success(),
        "writing the stub {} failed: {} {}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod stub");
}

/// See the unix arm. Other platforms have no `ETXTBSY` and no mode bits.
#[cfg(not(unix))]
pub fn write_stub_executable(path: &Path, script: &str) {
    std::fs::write(path, script).expect("write stub");
}

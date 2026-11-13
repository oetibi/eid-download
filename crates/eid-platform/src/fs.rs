//! File-system primitives with the safety properties the engine relies on.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Positional write that never touches a shared file cursor. Writes all of `buf`.
pub fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        #[cfg(unix)]
        let n = {
            use std::os::unix::fs::FileExt;
            file.write_at(buf, offset)?
        };
        #[cfg(windows)]
        let n = {
            use std::os::windows::fs::FileExt;
            file.seek_write(buf, offset)?
        };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write_at wrote 0 bytes",
            ));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// Positional read of up to `buf.len()` bytes.
pub fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        file.seek_read(buf, offset)
    }
}

#[cfg(unix)]
fn nofollow_flag() -> i32 {
    libc::O_NOFOLLOW
}

#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

fn reject_if_not_plain(f: &File) -> io::Result<()> {
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.nlink() > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing file with multiple hard links",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing reparse point",
            ));
        }
        if windows_link_count(f)? > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "refusing file with multiple hard links",
            ));
        }
    }
    Ok(())
}

/// NTFS hard-link count of an *open handle* (compile-checked on Linux, never executed here).
#[cfg(windows)]
fn windows_link_count(f: &File) -> io::Result<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // SAFETY: `info` is a plain-old-data out parameter; the handle is valid for the call.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.nNumberOfLinks)
}

/// Try to take an exclusive advisory lock on a file handle (`flock` / `LockFileEx`); the engine
/// takes it on the per-job `<name>.eid-lock` file, not on the data file. `Ok(false)` = another
/// job/process holds it. The lock lives as long as any duplicate of this handle stays open, so
/// in-flight blocking writes keep ownership until they finish.
pub fn try_lock_exclusive(f: &File) -> io::Result<bool> {
    match f.try_lock() {
        Ok(()) => Ok(true),
        Err(std::fs::TryLockError::WouldBlock) => Ok(false),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn apply_open_flags(o: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(nofollow_flag());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

/// Identity of an open file: (device, inode) on Unix, (volume serial, file index) on Windows.
type FileId = (u64, u64);

fn handle_id(f: &File) -> io::Result<FileId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = f.metadata()?;
        Ok((m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        // SAFETY: `i` is a plain-old-data out parameter; the handle is valid for the call.
        let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut i) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            u64::from(i.dwVolumeSerialNumber),
            (u64::from(i.nFileIndexHigh) << 32) | u64::from(i.nFileIndexLow),
        ))
    }
}

/// Identity of what `path` names right now (a final symlink / reparse point is NOT followed);
/// `None` when nothing is there.
fn path_id(path: &Path) -> io::Result<Option<FileId>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(m) => Ok(Some((m.dev(), m.ino()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let mut o = OpenOptions::new();
        // read access, share everything (incl. delete) so this probe never blocks the owner
        o.read(true)
            .share_mode(7)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match o.open(path) {
            Ok(g) => Ok(Some(handle_id(&g)?)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// True if `f` is the very file `path` names right now (same device+inode on Unix, same volume
/// serial + file index on Windows; `path` is not followed if it is a symlink). `false` when the
/// path no longer exists or names another file: an advisory lock taken on `f` then protects an
/// orphan, not the path (review M1: lock files are deleted while locked).
pub fn handle_is_path(f: &File, path: &Path) -> io::Result<bool> {
    Ok(path_id(path)? == Some(handle_id(f)?))
}

/// True if opening/locking a lock file failed because another party holds or is deleting it
/// (Windows: access denied for a delete-pending file, sharing/lock violation). Always `false` on
/// Unix. The engine maps this to "busy" instead of a generic I/O error (review M1).
pub fn is_lock_contention(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        matches!(e.raw_os_error(), Some(5 | 32 | 33))
    }
    #[cfg(not(windows))]
    {
        let _ = e;
        false
    }
}

/// Open (creating if needed) a per-job LOCK file: owner-only, never follows a symlink, must be a
/// plain single-link regular file. Used only for its advisory lock, never for data.
pub fn open_or_create_lock_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    apply_open_flags(&mut o);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(path)?;
    reject_if_not_plain(&f)?;
    Ok(f)
}

/// Create a brand-new regular file (fails if anything exists at `path`, including a
/// dangling symlink). Private (owner-only) permissions. This *claims* the name atomically.
pub fn create_new_private(path: &Path) -> io::Result<File> {
    create_new_impl(path, true)
}

/// Like [`create_new_private`] but with the normal (umask-derived) permissions: for user data.
pub fn create_new_file(path: &Path) -> io::Result<File> {
    create_new_impl(path, false)
}

fn create_new_impl(path: &Path, private: bool) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create_new(true);
    apply_open_flags(&mut o);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    // Windows: the file inherits the ACL of its parent directory (the user's profile /
    // chosen download dir). An explicit protected DACL is a P3 platform task (not done).
    let f = o.open(path)?;
    reject_if_not_plain(&f)?;
    Ok(f)
}

/// Open an EXISTING regular file read/write without following a final symlink, and verify
/// the *opened handle* is a plain single-link regular file (check after open, not before).
pub fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true);
    apply_open_flags(&mut o);
    let f = o.open(path)?;
    reject_if_not_plain(&f)?;
    Ok(f)
}

/// Read-only variant of [`open_regular_nofollow`].
pub fn open_regular_nofollow_read(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    apply_open_flags(&mut o);
    let f = o.open(path)?;
    reject_if_not_plain(&f)?;
    Ok(f)
}

/// Pre-size a file. On NTFS first marks it sparse (avoids the zero-fill stall when writing
/// far offsets first). Falls back to plain `set_len` where sparse is unsupported.
pub fn preallocate(file: &File, len: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
        use windows_sys::Win32::System::IO::DeviceIoControl;
        let mut returned = 0u32;
        // Best effort: FAT/exFAT/some network FS reject it; ignore the result.
        unsafe {
            let _ = DeviceIoControl(
                file.as_raw_handle() as _,
                FSCTL_SET_SPARSE,
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            );
        }
    }
    file.set_len(len)
}

/// Atomically replace `dest` with `tmp` (same directory). Windows: MoveFileEx with
/// REPLACE_EXISTING|WRITE_THROUGH and bounded retries (antivirus/indexer sharing violations).
pub fn replace_atomic(tmp: &Path, dest: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::rename(tmp, dest)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let w = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<u16>>()
        };
        let (a, b) = (w(tmp), w(dest));
        let mut last = io::Error::from(io::ErrorKind::Other);
        for attempt in 0..8u32 {
            let ok = unsafe {
                MoveFileExW(
                    a.as_ptr(),
                    b.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if ok != 0 {
                return Ok(());
            }
            last = io::Error::last_os_error();
            std::thread::sleep(std::time::Duration::from_millis(20 * (attempt as u64 + 1)));
        }
        Err(last)
    }
}

/// Rename that fails with `AlreadyExists` instead of overwriting `to`.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    rename_noreplace_hooked(from, to, &mut || {})
}

/// [`rename_noreplace`] with a hook that runs right after the new name exists (Unix: after the
/// link, before the old name is dropped; Windows: after the move). Test seam for crash points.
pub fn rename_noreplace_hooked(
    from: &Path,
    to: &Path,
    after_link: &mut dyn FnMut(),
) -> io::Result<()> {
    #[cfg(unix)]
    {
        // link() fails with EEXIST atomically; then drop the old name.
        std::fs::hard_link(from, to)?;
        after_link();
        std::fs::remove_file(from)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let w = |p: &Path| {
            p.as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<u16>>()
        };
        let (a, b) = (w(from), w(to));
        // Flags 0: no REPLACE_EXISTING => fails if `to` exists.
        if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 {
            after_link();
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

fn mismatch(msg: &str, part: &Path, dest: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{msg} (part: {}, destination: {})",
            part.display(),
            dest.display()
        ),
    )
}

/// Drop the part NAME, but only if it still names the verified file: a foreign entry that was
/// swapped in is left alone (never delete what is not provably ours).
#[cfg(unix)]
fn drop_part_if_ours(file: &File, part: &Path) -> io::Result<()> {
    if handle_is_path(file, part)? {
        match std::fs::remove_file(part) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    if let Some(dir) = part.parent() {
        let _ = sync_dir(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        });
    }
    Ok(())
}

/// Publish the VERIFIED file `file` (an open handle whose content the caller hashed) as `dest`,
/// never overwriting an existing `dest` (`AlreadyExists`), and drop its old name `part`.
///
/// * Linux: `linkat(/proc/self/fd/N, AT_SYMLINK_FOLLOW)` publishes the handle's own inode no
///   matter what `part` names now, so a swapped part name cannot change what is published.
/// * Elsewhere (and when /proc is unavailable): the source name is checked against the handle
///   before and the destination after promotion; a mismatch is refused, our own foreign-inode
///   link is undone, and anything we cannot prove is ours is left untouched. This DETECTS, it
///   does not prevent (a same-user process with directory write access can still race it).
pub fn publish_verified(file: &File, part: &Path, dest: &Path) -> io::Result<()> {
    publish_verified_hooked(file, part, dest, &mut || {})
}

/// [`publish_verified`] with a hook that runs right after the new name exists (crash seam).
pub fn publish_verified_hooked(
    file: &File,
    part: &Path,
    dest: &Path,
    after_link: &mut dyn FnMut(),
) -> io::Result<()> {
    publish_impl(file, part, dest, false, &mut || {}, after_link)
}

/// Should a failed `linkat` fall back to the checked path-based promotion? Only when the /proc
/// route itself is unavailable; EEXIST (name taken), EXDEV, ... propagate.
#[cfg(unix)]
fn linkat_error_selects_fallback(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::ENOENT | libc::EPERM | libc::EACCES)
    )
}

#[cfg(target_os = "linux")]
fn linkat_handle(file: &File, dest: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let src = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let dst = CString::new(dest.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: both pointers are valid NUL-terminated strings for the duration of the call.
    let r = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            src.as_ptr(),
            libc::AT_FDCWD,
            dst.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn publish_impl(
    file: &File,
    part: &Path,
    dest: &Path,
    force_fallback: bool,
    before_link: &mut dyn FnMut(),
    after_link: &mut dyn FnMut(),
) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    if !force_fallback {
        before_link();
        match linkat_handle(file, dest) {
            Ok(()) => {
                after_link();
                if !handle_is_path(file, dest)? {
                    return Err(mismatch(
                        "destination does not name the verified file after publication; left untouched",
                        part,
                        dest,
                    ));
                }
                return drop_part_if_ours(file, part);
            }
            Err(e) if linkat_error_selects_fallback(&e) => {}
            Err(e) => return Err(e),
        }
    }
    let _ = force_fallback;
    // checked path-based promotion
    if !handle_is_path(file, part)? {
        return Err(mismatch(
            "source name no longer refers to the verified file; nothing published",
            part,
            dest,
        ));
    }
    before_link();
    std::fs::hard_link(part, dest)?; // EEXIST -> AlreadyExists propagates
    let linked = path_id(dest).ok().flatten();
    after_link();
    let verified = handle_id(file)?;
    let now = path_id(dest)?;
    if now == Some(verified) {
        return drop_part_if_ours(file, part);
    }
    // identity mismatch: undo our link ONLY if `dest` and `part` are still two names of the same
    // foreign inode (the one our link() created): removing one of two names destroys no data.
    // Pathname snapshots cannot prove ownership atomically, so a stranger swapping `dest` in the
    // instant between this check and the unlink is a documented residual race.
    if linked.is_some()
        && linked != Some(verified)
        && now == linked
        && path_id(part).ok().flatten() == linked
    {
        let _ = std::fs::remove_file(dest);
        return Err(mismatch(
            "the source was swapped before promotion; our link to the foreign file was removed, nothing published",
            part,
            dest,
        ));
    }
    Err(mismatch(
        "destination changed during promotion; everything left untouched",
        part,
        dest,
    ))
}

#[cfg(windows)]
fn publish_impl(
    file: &File,
    part: &Path,
    dest: &Path,
    _force_fallback: bool,
    before_link: &mut dyn FnMut(),
    after_link: &mut dyn FnMut(),
) -> io::Result<()> {
    if !handle_is_path(file, part)? {
        return Err(mismatch(
            "source name no longer refers to the verified file; nothing published",
            part,
            dest,
        ));
    }
    before_link();
    rename_noreplace(part, dest)?; // AlreadyExists propagates
    let moved = path_id(dest).ok().flatten();
    after_link();
    let verified = handle_id(file)?;
    let now = path_id(dest)?;
    if now == Some(verified) {
        return Ok(());
    }
    // roll back only if `dest` still names exactly the (foreign) entry we moved
    if moved.is_some() && moved != Some(verified) && now == moved {
        return match rename_noreplace(dest, part) {
            Ok(()) => Err(mismatch(
                "the source was swapped before promotion; moved back, nothing published",
                part,
                dest,
            )),
            Err(e) => Err(mismatch(
                &format!("the source was swapped before promotion and rollback failed ({e}); both names left as they are"),
                part,
                dest,
            )),
        };
    }
    Err(mismatch(
        "destination changed during promotion; everything left untouched",
        part,
        dest,
    ))
}

/// Crash recovery only (Unix): if `part` and `dest` are two names of ONE regular file (same
/// device+inode, any link count), return a read handle to it; otherwise `None` (different
/// inode, symlink, FIFO/device, missing, non-Unix). Neither name is followed if it is a symlink
/// and opening never blocks (`O_NONBLOCK`: a FIFO planted at either name is rejected, not
/// waited on). Call from a blocking thread. Callers must still require
/// [`recovery_pair_is_exact`] before acting on the handle.
pub fn open_linked_pair_for_recovery(part: &Path, dest: &Path) -> io::Result<Option<File>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let open = |p: &Path| -> io::Result<Option<File>> {
            let mut o = OpenOptions::new();
            o.read(true)
                .custom_flags(nofollow_flag() | libc::O_NONBLOCK);
            match o.open(p) {
                Ok(f) => Ok(Some(f)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                // O_NOFOLLOW on a symlink: ELOOP -> "not a candidate"
                Err(e) if e.raw_os_error() == Some(libc::ELOOP) => Ok(None),
                Err(e) => Err(e),
            }
        };
        let (Some(a), Some(b)) = (open(part)?, open(dest)?) else {
            return Ok(None);
        };
        let (ma, mb) = (a.metadata()?, b.metadata()?);
        let same = ma.is_file() && mb.is_file() && (ma.dev(), ma.ino()) == (mb.dev(), mb.ino());
        Ok(same.then_some(a))
    }
    #[cfg(not(unix))]
    {
        let _ = (part, dest);
        Ok(None)
    }
}

/// Crash recovery only: `h` (from [`open_linked_pair_for_recovery`]) still names BOTH `part` and
/// `dest` right now and has exactly two links. Re-checked after hashing, right before the
/// leftover part name is dropped. Always `false` off Unix (the Windows promotion is one move).
pub fn recovery_pair_is_exact(h: &File, part: &Path, dest: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(h.metadata()?.nlink() == 2 && handle_is_path(h, part)? && handle_is_path(h, dest)?)
    }
    #[cfg(not(unix))]
    {
        let _ = (h, part, dest);
        Ok(false)
    }
}

/// fsync a directory so a rename is durable (no-op on Windows where NTFS journals metadata).
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(())
    }
}

/// Create a symlink to a file (used by security tests). Fails loudly if not permitted.
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// True if the file permits access to its owner only (Unix mode bits; on Windows this
/// returns `None` = "not verified": real ACL inspection is a P3 task).
pub fn owner_only(path: &Path) -> io::Result<Option<bool>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(Some(
            std::fs::metadata(path)?.permissions().mode() & 0o077 == 0,
        ))
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("eid-plat-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn positional_writes_out_of_order() {
        let d = tmpdir("pw");
        let f = create_new_private(&d.join("a")).unwrap();
        preallocate(&f, 10).unwrap();
        write_all_at(&f, b"world", 5).unwrap();
        write_all_at(&f, b"hello", 0).unwrap();
        assert_eq!(std::fs::read(d.join("a")).unwrap(), b"helloworld");
    }

    #[test]
    fn security_create_new_refuses_existing_and_dangling_symlink() {
        let d = tmpdir("cn");
        std::fs::write(d.join("x"), b"1").unwrap();
        assert!(create_new_private(&d.join("x")).is_err());
        let outside = d.join("outside-target");
        symlink_file(&outside, &d.join("dangling")).expect("symlink creation must work in tests");
        assert!(create_new_private(&d.join("dangling")).is_err());
        assert!(!outside.exists(), "dangling symlink must not be followed");
    }

    #[test]
    fn security_open_regular_nofollow_rejects_symlink_and_hardlink() {
        let d = tmpdir("nf");
        let outside = d.join("outside");
        std::fs::write(&outside, b"SECRET").unwrap();
        symlink_file(&outside, &d.join("link")).expect("symlink creation must work in tests");
        assert!(open_regular_nofollow(&d.join("link")).is_err());
        #[cfg(unix)]
        {
            std::fs::hard_link(&outside, d.join("hard")).unwrap();
            assert!(open_regular_nofollow(&d.join("hard")).is_err());
        }
        assert_eq!(std::fs::read(&outside).unwrap(), b"SECRET");
    }

    #[test]
    fn rename_noreplace_does_not_overwrite() {
        let d = tmpdir("rn");
        std::fs::write(d.join("a"), b"a").unwrap();
        std::fs::write(d.join("b"), b"b").unwrap();
        assert!(rename_noreplace(&d.join("a"), &d.join("b")).is_err());
        assert_eq!(std::fs::read(d.join("b")).unwrap(), b"b");
        assert!(d.join("a").exists());
        rename_noreplace(&d.join("a"), &d.join("c")).unwrap();
        assert!(!d.join("a").exists());
    }

    #[test]
    fn security_private_file_is_owner_only() {
        let d = tmpdir("pv");
        create_new_private(&d.join("s")).unwrap();
        #[cfg(unix)]
        assert_eq!(owner_only(&d.join("s")).unwrap(), Some(true));
    }

    #[test]
    fn replace_atomic_replaces() {
        let d = tmpdir("ra");
        std::fs::write(d.join("t"), b"new").unwrap();
        std::fs::write(d.join("d"), b"old").unwrap();
        replace_atomic(&d.join("t"), &d.join("d")).unwrap();
        assert_eq!(std::fs::read(d.join("d")).unwrap(), b"new");
    }

    // ---- M2: publish_verified ------------------------------------------------------------

    /// A verified part file `<d>/p` with `bytes`, kept open (the "verified handle").
    fn verified_part(d: &Path, bytes: &[u8]) -> (File, std::path::PathBuf) {
        let part = d.join("p");
        let f = create_new_private(&part).unwrap();
        write_all_at(&f, bytes, 0).unwrap();
        (f, part)
    }

    /// Move the part aside (the verified inode lives on under `aside`) and plant `evil` at its name.
    fn swap_part(d: &Path, part: &Path, evil: &[u8]) -> std::path::PathBuf {
        let aside = d.join("aside");
        std::fs::rename(part, &aside).unwrap();
        std::fs::write(part, evil).unwrap();
        aside
    }

    #[test]
    fn publish_verified_publishes_the_verified_inode_and_removes_the_source() {
        let d = tmpdir("pv1");
        let (f, part) = verified_part(&d, b"ORIG");
        let dest = d.join("dest");
        publish_verified(&f, &part, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"ORIG");
        assert!(!part.exists(), "the part name must be gone");
        assert!(
            handle_is_path(&f, &dest).unwrap(),
            "dest is not the verified inode"
        );
    }

    #[test]
    fn publish_verified_noreplace_keeps_existing_destination() {
        let d = tmpdir("pv2");
        let (f, part) = verified_part(&d, b"ORIG");
        let dest = d.join("dest");
        std::fs::write(&dest, b"THEIRS").unwrap();
        let e = publish_verified(&f, &part, &dest).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"THEIRS");
        assert_eq!(std::fs::read(&part).unwrap(), b"ORIG", "part must be kept");
    }

    #[test]
    fn publish_verified_fallback_refuses_a_source_replaced_before_promotion() {
        let d = tmpdir("pv4");
        let (f, part) = verified_part(&d, b"ORIG");
        let aside = swap_part(&d, &part, b"EVIL");
        let dest = d.join("dest");
        let e = publish_impl(&f, &part, &dest, true, &mut || {}, &mut || {}).unwrap_err();
        assert!(e.to_string().contains("no longer refers"), "{e}");
        assert!(!dest.exists(), "nothing may be published");
        assert_eq!(std::fs::read(&part).unwrap(), b"EVIL", "substitute touched");
        assert_eq!(std::fs::read(&aside).unwrap(), b"ORIG");
        // and the success path of the fallback itself
        let d2 = tmpdir("pv4b");
        let (f2, part2) = verified_part(&d2, b"ORIG");
        publish_impl(&f2, &part2, &d2.join("dest"), true, &mut || {}, &mut || {}).unwrap();
        assert_eq!(std::fs::read(d2.join("dest")).unwrap(), b"ORIG");
        assert!(!part2.exists());
    }

    #[test]
    fn publish_verified_fallback_rolls_back_a_swap_between_link_and_post_check() {
        // (A) the source is swapped AFTER the pre-check, BEFORE the link: we link the foreign
        // file; the post-check refuses and removes exactly our own link; the foreign part stays.
        let d = tmpdir("pv5a");
        let (f, part) = verified_part(&d, b"ORIG");
        let dest = d.join("dest");
        let (pp, dd) = (part.clone(), d.clone());
        let e = publish_impl(
            &f,
            &part,
            &dest,
            true,
            &mut || {
                swap_part(&dd, &pp, b"EVIL");
            },
            &mut || {},
        )
        .unwrap_err();
        assert!(e.to_string().contains("swapped before promotion"), "{e}");
        assert!(
            !dest.exists(),
            "our link to the foreign file must be removed"
        );
        assert_eq!(
            std::fs::read(&part).unwrap(),
            b"EVIL",
            "foreign part touched"
        );

        // (B) the DESTINATION is replaced by someone else after our link: never delete it.
        let d = tmpdir("pv5b");
        let (f, part) = verified_part(&d, b"ORIG");
        let dest = d.join("dest");
        let (dp, dd) = (dest.clone(), d.clone());
        let e = publish_impl(&f, &part, &dest, true, &mut || {}, &mut || {
            std::fs::rename(&dp, dd.join("moved.orig")).unwrap();
            std::fs::write(&dp, b"FOREIGN").unwrap();
        })
        .unwrap_err();
        assert!(e.to_string().contains("everything left untouched"), "{e}");
        assert_eq!(
            std::fs::read(&dest).unwrap(),
            b"FOREIGN",
            "foreign dest deleted"
        );
        #[cfg(unix)] // Windows moved the part away; its name is gone by design
        assert_eq!(std::fs::read(&part).unwrap(), b"ORIG", "part must be kept");
    }

    #[cfg(unix)]
    #[test]
    fn publish_verified_fallback_error_selection() {
        let e = |c: i32| io::Error::from_raw_os_error(c);
        for c in [libc::ENOENT, libc::EPERM, libc::EACCES] {
            assert!(
                linkat_error_selects_fallback(&e(c)),
                "errno {c} must fall back"
            );
        }
        for c in [libc::EXDEV, libc::EEXIST, libc::EIO, libc::ENOSPC] {
            assert!(
                !linkat_error_selects_fallback(&e(c)),
                "errno {c} must propagate"
            );
        }
        // EEXIST through the fallback path surfaces as AlreadyExists (numbered-name loop)
        let d = tmpdir("pv6");
        let (f, part) = verified_part(&d, b"ORIG");
        std::fs::write(d.join("dest"), b"X").unwrap();
        let r = publish_impl(&f, &part, &d.join("dest"), true, &mut || {}, &mut || {});
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(d.join("dest")).unwrap(), b"X");
    }

    #[cfg(unix)]
    #[test]
    fn publish_verified_linkat_publishes_original_even_if_part_name_swapped() {
        let d = tmpdir("pv7");
        let (f, part) = verified_part(&d, b"ORIG");
        let aside = swap_part(&d, &part, b"EVIL");
        let dest = d.join("dest");
        let r = publish_verified(&f, &part, &dest);
        #[cfg(target_os = "linux")]
        {
            r.unwrap();
            assert_eq!(
                std::fs::read(&dest).unwrap(),
                b"ORIG",
                "the substitute was published"
            );
            assert!(handle_is_path(&f, &dest).unwrap());
        }
        #[cfg(not(target_os = "linux"))]
        {
            // no /proc route: detection only, the substitute is never published
            assert!(r.is_err());
            assert!(!dest.exists());
        }
        assert_eq!(
            std::fs::read(&part).unwrap(),
            b"EVIL",
            "foreign part name touched"
        );
        assert_eq!(std::fs::read(&aside).unwrap(), b"ORIG");
    }

    /// Windows runtime check of the rollback-FAILURE path (compile-checked on Linux; executed
    /// only by ci-windows): the source name is re-occupied after the move, so the non-destructive
    /// rollback cannot happen and both names must be preserved.
    #[cfg(windows)]
    #[test]
    fn windows_rollback_failure_preserves_both_names() {
        let d = tmpdir("pvw");
        let (f, part) = verified_part(&d, b"ORIG");
        let dest = d.join("dest");
        let (pp, dd) = (part.clone(), d.clone());
        let pp2 = part.clone();
        let e = publish_impl(
            &f,
            &part,
            &dest,
            true,
            &mut || {
                swap_part(&dd, &pp, b"EVIL");
            },
            &mut || std::fs::write(&pp2, b"OCCUPIED").unwrap(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("rollback failed"), "{e}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"EVIL");
        assert_eq!(std::fs::read(&part).unwrap(), b"OCCUPIED");
    }

    #[cfg(unix)]
    #[test]
    fn publish_verified_refuses_a_symlink_substituted_for_the_source() {
        for force_fallback in [false, true] {
            let d = tmpdir(&format!("pv8{force_fallback}"));
            let outside = d.join("outside");
            std::fs::write(&outside, b"SECRET").unwrap();
            let (f, part) = verified_part(&d, b"ORIG");
            std::fs::rename(&part, d.join("aside")).unwrap();
            symlink_file(&outside, &part).unwrap();
            let dest = d.join("dest");
            let r = publish_impl(&f, &part, &dest, force_fallback, &mut || {}, &mut || {});
            if let Ok(md) = std::fs::symlink_metadata(&dest) {
                assert!(!md.file_type().is_symlink(), "a symlink was published");
                assert_eq!(std::fs::read(&dest).unwrap(), b"ORIG");
            } else {
                assert!(r.is_err());
            }
            assert!(!force_fallback || !dest.exists(), "fallback must refuse");
            assert_eq!(std::fs::read(&outside).unwrap(), b"SECRET");
            assert!(std::fs::symlink_metadata(&part)
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_open_never_blocks_on_a_fifo() {
        use std::os::unix::ffi::OsStrExt;
        let d = tmpdir("fifo");
        let (p, q) = (d.join("p"), d.join("q"));
        std::fs::write(&p, b"A").unwrap();
        let c = std::ffi::CString::new(q.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path for the duration of the call.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        for (a, b) in [(p.clone(), q.clone()), (q.clone(), p.clone())] {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(open_linked_pair_for_recovery(&a, &b).map(|o| o.is_none()));
            });
            let r = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("opening a FIFO blocked");
            assert!(r.unwrap(), "a FIFO must not be a recovery candidate");
        }
    }

    #[test]
    fn recovery_open_rejects_extra_links_and_different_inode() {
        let d = tmpdir("rec");
        let (p, q) = (d.join("p"), d.join("q"));
        std::fs::write(&p, b"A").unwrap();
        std::fs::write(&q, b"A").unwrap();
        assert!(
            open_linked_pair_for_recovery(&p, &q).unwrap().is_none(),
            "different inodes"
        );
        assert!(open_linked_pair_for_recovery(&p, &d.join("missing"))
            .unwrap()
            .is_none());
        #[cfg(unix)]
        {
            let r = d.join("r");
            std::fs::remove_file(&q).unwrap();
            std::fs::hard_link(&p, &q).unwrap();
            let h = open_linked_pair_for_recovery(&p, &q)
                .unwrap()
                .expect("exactly two names");
            assert!(handle_is_path(&h, &p).unwrap());
            assert!(recovery_pair_is_exact(&h, &p, &q).unwrap());
            std::fs::hard_link(&p, &r).unwrap(); // third name
            assert!(
                !recovery_pair_is_exact(&h, &p, &q).unwrap(),
                "three links must not be exact"
            );
            std::fs::remove_file(&r).unwrap();
            // a symlink named like the destination is never followed
            let s = d.join("s");
            symlink_file(&p, &s).unwrap();
            assert!(
                open_linked_pair_for_recovery(&p, &s).unwrap().is_none(),
                "symlink"
            );
        }
    }
}

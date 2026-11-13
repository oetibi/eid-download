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
        // Hard-link count on Windows needs GetFileInformationByHandle: NOT implemented yet
        // (listed in docs/STATUS.md as untested/unimplemented on Windows).
    }
    Ok(())
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
    #[cfg(unix)]
    {
        // link() fails with EEXIST atomically; then drop the old name.
        std::fs::hard_link(from, to)?;
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
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
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
}

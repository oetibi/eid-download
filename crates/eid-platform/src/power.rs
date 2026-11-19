//! v0.2: what the computer does when the downloads are finished (sleep, hibernate, shut down).
//!
//! Only ever called by the desktop app after its cancellable on-screen countdown
//! (`eid_engine::scheduler`, 60 s, aborted as soon as new work appears). Windows only; elsewhere
//! every action reports "not available". Nothing here is run by the tests except the privilege
//! step, which changes no power state.

/// Put the computer to sleep.
pub fn sleep() -> std::io::Result<()> {
    imp::suspend(false)
}

/// Hibernate (fails if hibernation is turned off in Windows).
pub fn hibernate() -> std::io::Result<()> {
    imp::suspend(true)
}

/// Shut down and power off. Programs get the normal end-of-session messages (not forced).
pub fn shutdown() -> std::io::Result<()> {
    imp::shutdown()
}

/// Enable the shutdown privilege of this process (needed by all three actions). Changes no
/// power state; used by the tests to check the Windows calls.
pub fn enable_shutdown_privilege() -> std::io::Result<()> {
    imp::enable_privilege()
}

#[cfg(windows)]
mod imp {
    use std::io::{Error, Result};
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_SUCCESS, HANDLE, LUID};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Power::SetSuspendState;
    use windows_sys::Win32::System::Shutdown::{
        ExitWindowsEx, EWX_POWEROFF, EWX_SHUTDOWN, SHTDN_REASON_FLAG_PLANNED,
        SHTDN_REASON_MAJOR_APPLICATION, SHTDN_REASON_MINOR_OTHER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn enable_privilege() -> Result<()> {
        let name: Vec<u16> = "SeShutdownPrivilege".encode_utf16().chain([0]).collect();
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of this process; `token` receives a handle we close below.
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        } == 0
        {
            return Err(Error::last_os_error());
        }
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: `name` is NUL-terminated UTF-16; `luid` is a valid out pointer.
        let ok = unsafe { LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) };
        let r = if ok == 0 {
            Err(Error::last_os_error())
        } else {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [windows_sys::Win32::Security::LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            // SAFETY: `token` is open with TOKEN_ADJUST_PRIVILEGES; no previous state requested.
            let ok = unsafe {
                AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut())
            };
            // AdjustTokenPrivileges succeeds even when the privilege was not assigned
            // (ERROR_NOT_ALL_ASSIGNED): only ERROR_SUCCESS means it is really enabled.
            // SAFETY: plain query of this thread's last error.
            let last = unsafe { GetLastError() };
            if ok == 0 || last != ERROR_SUCCESS {
                Err(Error::from_raw_os_error(last as i32))
            } else {
                Ok(())
            }
        };
        // SAFETY: opened above.
        unsafe { CloseHandle(token) };
        r
    }

    pub fn suspend(hibernate: bool) -> Result<()> {
        enable_privilege()?;
        // SAFETY: plain call; wake events stay enabled (last argument false).
        if unsafe { SetSuspendState(u8::from(hibernate), 0, 0) } == 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }

    pub fn shutdown() -> Result<()> {
        enable_privilege()?;
        // SAFETY: plain call; not forced, so programs may ask the user to save their work.
        let ok = unsafe {
            ExitWindowsEx(
                EWX_SHUTDOWN | EWX_POWEROFF,
                SHTDN_REASON_MAJOR_APPLICATION
                    | SHTDN_REASON_MINOR_OTHER
                    | SHTDN_REASON_FLAG_PLANNED,
            )
        };
        if ok == 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::io::{Error, ErrorKind, Result};
    fn na() -> Error {
        Error::new(ErrorKind::Unsupported, "power actions are Windows-only")
    }
    pub fn enable_privilege() -> Result<()> {
        Err(na())
    }
    pub fn suspend(_hibernate: bool) -> Result<()> {
        Err(na())
    }
    pub fn shutdown() -> Result<()> {
        Err(na())
    }
}

#[cfg(test)]
mod tests {
    /// Windows: the privilege step (the part every action needs) works for a normal user.
    /// It changes no power state. Fails, never skips, if it does not work.
    #[cfg(windows)]
    #[test]
    fn windows_shutdown_privilege_can_be_enabled() {
        super::enable_shutdown_privilege().expect("SeShutdownPrivilege");
    }

    #[cfg(not(windows))]
    #[test]
    fn other_systems_report_power_actions_as_not_available() {
        assert!(super::sleep().is_err());
        assert!(super::hibernate().is_err());
        assert!(super::shutdown().is_err());
        assert!(super::enable_shutdown_privilege().is_err());
    }
}

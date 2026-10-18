//! pdf-extract prints debug lines to stdout on some PDFs. For machine-readable
//! modes (stdio protocol, --json) we keep a private handle to the real stdout
//! and point the process's stdout at stderr, so stray prints cannot corrupt
//! the output.

use std::fs::File;
use std::io;

pub fn isolate_stdout() -> io::Result<File> {
    imp::isolate()
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::fd::FromRawFd;

    pub fn isolate() -> io::Result<File> {
        // SAFETY: plain fd syscalls; `saved` is a fresh fd we own.
        unsafe {
            let saved = libc::dup(libc::STDOUT_FILENO);
            if saved < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
                let e = io::Error::last_os_error();
                libc::close(saved);
                return Err(e);
            }
            Ok(File::from_raw_fd(saved))
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn isolate() -> io::Result<File> {
        // SAFETY: Win32 handle calls; `dup` is a fresh handle we own.
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            if out.is_null() || out == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut dup: HANDLE = std::ptr::null_mut();
            let process = GetCurrentProcess();
            if DuplicateHandle(process, out, process, &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
                return Err(io::Error::last_os_error());
            }
            // Rust's stdout looks up the handle on every write, so this redirects it.
            SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE));
            Ok(File::from_raw_handle(dup as _))
        }
    }
}

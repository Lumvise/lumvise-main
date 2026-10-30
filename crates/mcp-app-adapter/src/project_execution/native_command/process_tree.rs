//! Owns the OS lifetime boundary for one native job and all inherited children.
use std::io;
use tokio::process::{Child, Command};

pub(super) struct NativeCommandTree {
    closed: bool,
    #[cfg(unix)]
    process_group: libc::pid_t,
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

impl NativeCommandTree {
    pub(super) fn configure(command: &mut Command) {
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(not(unix))]
        let _ = command;
    }

    pub(super) fn attach(child: &Child) -> io::Result<Self> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("native child has no process id"))?;
        #[cfg(unix)]
        return Ok(Self {
            closed: false,
            process_group: pid as libc::pid_t,
        });
        #[cfg(windows)]
        return windows_job(child).map(|job| Self {
            closed: false,
            _job: job,
        });
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            Ok(Self { closed: false })
        }
    }

    pub(super) async fn close(&mut self) {
        if self.closed {
            return;
        }
        #[cfg(unix)]
        {
            self.signal(libc::SIGTERM);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
            while self.is_running() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            self.signal(libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            // Closing a completed job must also terminate children holding output pipes.
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(
                    self._job.as_raw_handle(),
                    1,
                );
            }
        }
        self.closed = true;
    }

    #[cfg(unix)]
    fn signal(&self, signal: libc::c_int) {
        // The child creates its own group before exec; never signal our caller's group.
        unsafe {
            libc::kill(-self.process_group, signal);
        }
    }

    #[cfg(unix)]
    fn is_running(&self) -> bool {
        unsafe { libc::kill(-self.process_group, 0) == 0 }
    }
}

impl Drop for NativeCommandTree {
    fn drop(&mut self) {
        #[cfg(unix)]
        if !self.closed {
            self.signal(libc::SIGKILL);
        }
        // Windows closes the kill-on-close Job Object through OwnedHandle.
    }
}

#[cfg(windows)]
fn windows_job(child: &Child) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::*;
    let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw_job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    if configured == 0 {
        return Err(io::Error::last_os_error());
    }
    let process = child
        .raw_handle()
        .ok_or_else(|| io::Error::other("native child has no process handle"))?;
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

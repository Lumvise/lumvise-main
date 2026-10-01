#![cfg(target_os = "windows")]

use std::io;
use std::path::{Path, PathBuf};
use std::ptr;

use widestring::U16CString;
use windows_sys::Win32::{
    Foundation::{GetLastError, HLOCAL, LocalFree},
    Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    },
    Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    },
};

const OWNER_SYSTEM_DACL: &str = "D:P(A;;GA;;;OW)(A;;GA;;;SY)";

struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0 as HLOCAL);
            }
        }
    }
}

fn path_error(path: &Path, operation: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{operation} '{}': {error}", path.display()),
    )
}

fn win32_path_error(path: &Path, operation: &str, code: u32) -> io::Error {
    path_error(path, operation, io::Error::from_raw_os_error(code as i32))
}

pub(crate) fn prepare_runtime_root(root: &Path) -> io::Result<()> {
    std::fs::create_dir_all(root)
        .map_err(|error| path_error(root, "create runtime directory", error))?;
    restrict_owner_access(root)
}

pub(crate) fn restrict_owner_access(path: &Path) -> io::Result<()> {
    let path_wide = U16CString::from_os_str(path.as_os_str()).map_err(|error| {
        path_error(
            path,
            "encode path for ACL hardening",
            io::Error::new(io::ErrorKind::InvalidInput, error.to_string()),
        )
    })?;
    let sddl = U16CString::from_str(OWNER_SYSTEM_DACL).map_err(|error| {
        path_error(
            path,
            "encode ACL security descriptor",
            io::Error::new(io::ErrorKind::InvalidInput, error.to_string()),
        )
    })?;

    let mut descriptor = SecurityDescriptor(ptr::null_mut());
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor.0,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        let code = unsafe { GetLastError() };
        return Err(win32_path_error(
            path,
            "convert ACL security descriptor",
            code,
        ));
    }

    let security_information = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
    let applied =
        unsafe { SetFileSecurityW(path_wide.as_ptr(), security_information, descriptor.0) };
    if applied == 0 {
        let code = unsafe { GetLastError() };
        return Err(win32_path_error(path, "apply protected ACL", code));
    }
    Ok(())
}

pub(crate) fn lease_path(root: &Path) -> PathBuf {
    root.join("owner.lock")
}

pub(crate) fn launch_desktop(request: &super::ActivationRequest) -> std::io::Result<()> {
    let executable = std::env::var_os("LUMVISE_EXECUTABLE").unwrap_or_else(|| "Lumvise.exe".into());
    std::process::Command::new(executable)
        .args(&request.arguments)
        .spawn()
        .map(|_| ())
}

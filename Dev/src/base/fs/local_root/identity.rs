//! Root-relative path decomposition and stable filesystem-entry identity.

use std::ffi::OsStr;

use crate::foundation::path::{EntryName, RootRelativeDir};

pub(super) trait EntryNameOsStr {
    fn as_os_str(&self) -> &OsStr;
}

impl EntryNameOsStr for EntryName {
    fn as_os_str(&self) -> &OsStr {
        OsStr::new(self.as_str())
    }
}

pub(super) fn segments(relative: &str) -> impl Iterator<Item = &str> {
    relative.split('/').filter(|segment| !segment.is_empty())
}

pub(super) fn split_parent(relative: &str) -> (RootRelativeDir, EntryName) {
    let (parent, name) = relative
        .rsplit_once('/')
        .map_or(("", relative), |(parent, name)| (parent, name));
    (
        RootRelativeDir::try_from(parent)
            .expect("validated root-relative paths have a valid parent directory"),
        EntryName::try_from(name)
            .expect("the final segment of a validated root-relative path is an entry name"),
    )
}

pub(super) fn random_token() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| {
        std::io::Error::other(format!("random token generation failed: {error}"))
    })?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(token, "{byte:02x}").expect("writing into a String cannot fail");
    }
    Ok(token)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct FileIdentity {
    pub(super) device: u64,
    pub(super) file: u64,
}

#[cfg(unix)]
pub(super) fn file_identity(file: &std::fs::File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(FileIdentity {
        device: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
pub(super) fn file_identity(file: &std::fs::File) -> std::io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(FileIdentity {
        device: u64::from(information.dwVolumeSerialNumber),
        file: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_identity(_file: &std::fs::File) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable directory identity is unavailable on this platform",
    ))
}

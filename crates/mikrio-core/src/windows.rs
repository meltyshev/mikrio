//! Handle-relative Windows filesystem effects and native identity.

#![allow(
    unsafe_code,
    reason = "Windows handle-relative operations require OS FFI"
)]

use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation, NtCreateFile,
    NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO_EX,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfoEx, FileIdInfo,
    GetFileInformationByHandle, GetFileInformationByHandleEx, SYNCHRONIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

const SHARING: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileIdentity(u64, [u8; 16]);

pub(crate) fn open_directory(path: &Path) -> io::Result<File> {
    // FlushFileBuffers requires write access, including for an NTFS directory.
    // Null security attributes in std::fs keep this handle noninheritable.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(SHARING)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;

    validate_directory(&file)?;
    Ok(file)
}

pub(crate) fn open_child(directory: &File, name: &str) -> io::Result<File> {
    let file = open_relative(
        directory,
        name,
        GENERIC_READ | GENERIC_WRITE,
        FILE_OPEN,
        FILE_DIRECTORY_FILE,
    )?;

    validate_directory(&file)?;
    Ok(file)
}

pub(crate) fn create_file(directory: &File, name: &str) -> io::Result<File> {
    let file = open_relative(
        directory,
        name,
        GENERIC_READ | GENERIC_WRITE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE,
    )?;

    validate_file(&file)?;
    Ok(file)
}

pub(crate) fn open_file(directory: &File, name: &str) -> io::Result<File> {
    open_existing_file(directory, name, GENERIC_READ)
}

pub(crate) fn open_writable_file(directory: &File, name: &str) -> io::Result<File> {
    open_existing_file(directory, name, GENERIC_READ | GENERIC_WRITE)
}

fn open_existing_file(directory: &File, name: &str, access: u32) -> io::Result<File> {
    let file = open_relative(directory, name, access, FILE_OPEN, FILE_NON_DIRECTORY_FILE)?;
    validate_file(&file)?;
    Ok(file)
}

fn open_relative(
    directory: &File,
    name: &str,
    access: u32,
    disposition: u32,
    kind: u32,
) -> io::Result<File> {
    let mut name = basename(name)?;
    let length = u16::try_from(name.len() * size_of::<u16>()).map_err(io::Error::other)?;

    let mut name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };

    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).map_err(io::Error::other)?,
        RootDirectory: directory.as_raw_handle(),
        ObjectName: &raw mut name,
        // No OBJ_INHERIT: a spawned process must not retain store protection.
        Attributes: OBJ_CASE_INSENSITIVE,
        ..Default::default()
    };

    let mut handle = ptr::null_mut();
    let mut status = IO_STATUS_BLOCK::default();

    // SAFETY: all buffers and the borrowed root handle remain live for this
    // synchronous call. The single basename cannot traverse another component;
    // OPEN_REPARSE_POINT opens the entry itself for validation before use.
    let result = unsafe {
        NtCreateFile(
            &raw mut handle,
            access | SYNCHRONIZE,
            &raw const attributes,
            &raw mut status,
            ptr::null(),
            0,
            SHARING,
            disposition,
            kind | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };

    nt_result(result)?;

    // SAFETY: successful NtCreateFile transfers one valid, uniquely owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn basename(name: &str) -> io::Result<Vec<u16>> {
    if name.is_empty() || name.contains(['/', '\\', ':', '\0']) || name.ends_with(['.', ' ']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a basename",
        ));
    }

    let name: Vec<_> = name.encode_utf16().collect();

    if name.len() > usize::from(u16::MAX) / size_of::<u16>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "basename is too long",
        ));
    }

    Ok(name)
}

fn validate_directory(file: &File) -> io::Result<()> {
    let attributes = file.metadata()?.file_attributes();

    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a real directory",
        ));
    }

    Ok(())
}

fn validate_file(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;

    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a regular file",
        ));
    }

    // Payload recovery may retain staging and published names for one inode.
    // Only mutable metadata and guard identities require a single hard link.
    Ok(())
}

pub(crate) fn validate_single_file(file: &File) -> io::Result<()> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();

    // SAFETY: file owns a live handle; information is an aligned writable output
    // of exactly the type required by the synchronous API.
    let result = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) };

    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || information.nNumberOfLinks != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a single regular file",
        ));
    }

    Ok(())
}

pub(crate) fn identity(file: &File) -> io::Result<FileIdentity> {
    let mut information = FILE_ID_INFO::default();
    let size = u32::try_from(size_of::<FILE_ID_INFO>()).map_err(io::Error::other)?;

    // SAFETY: the output and byte count match FILE_ID_INFO; the borrowed handle
    // remains valid throughout the synchronous query. Retain all 128 ID bits.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut information).cast(),
            size,
        )
    };

    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(FileIdentity(
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
}

pub(crate) fn path_file_identity(path: &Path) -> io::Result<FileIdentity> {
    let file = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARING)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;

    validate_single_file(&file)?;
    identity(&file)
}

#[cfg(test)]
pub(crate) fn allocated_bytes(path: &Path) -> io::Result<u64> {
    use windows_sys::Win32::Storage::FileSystem::{FILE_STANDARD_INFO, FileStandardInfo};

    let file = File::open(path)?;
    let mut information = FILE_STANDARD_INFO::default();
    let size = u32::try_from(size_of::<FILE_STANDARD_INFO>()).map_err(io::Error::other)?;

    // SAFETY: the live handle and typed, sized output agree with this information
    // class. The synchronous call writes only the supplied stack record.
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileStandardInfo,
            (&raw mut information).cast(),
            size,
        )
    };

    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    u64::try_from(information.AllocationSize).map_err(io::Error::other)
}

pub(crate) fn rename_new(
    source: &File,
    from: &str,
    destination: &File,
    to: &str,
) -> io::Result<()> {
    let file = open_existing_file(source, from, DELETE | FILE_READ_ATTRIBUTES)?;
    let name = basename(to)?;
    let name_bytes = name.len() * size_of::<u16>();
    let length = offset_of!(FILE_RENAME_INFORMATION, FileName) + name_bytes;

    let words = length
        .max(size_of::<FILE_RENAME_INFORMATION>())
        .div_ceil(size_of::<usize>());

    let mut storage = vec![0_usize; words];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let length = u32::try_from(length).map_err(io::Error::other)?;
    let name_bytes = u32::try_from(name_bytes).map_err(io::Error::other)?;
    let mut status = IO_STATUS_BLOCK::default();

    // SAFETY: usize storage has the structure's pointer alignment and enough
    // initialized bytes for its header and UTF-16 tail. Raw writes avoid forming
    // a reference to the one-element declaration of the variable-length tail.
    // Both directory and source handles outlive this synchronous call. Zeroed
    // ReplaceIfExists preserves an existing destination, with no copy fallback.
    let result = unsafe {
        ptr::addr_of_mut!((*information).RootDirectory).write(destination.as_raw_handle());
        ptr::addr_of_mut!((*information).FileNameLength).write(name_bytes);

        ptr::addr_of_mut!((*information).FileName)
            .cast::<u16>()
            .copy_from_nonoverlapping(name.as_ptr(), name.len());

        NtSetInformationFile(
            file.as_raw_handle(),
            &raw mut status,
            information.cast(),
            length,
            FileRenameInformation,
        )
    };

    nt_result(result)
}

pub(crate) fn unlink(directory: &File, name: &str) -> io::Result<()> {
    let file = open_existing_file(directory, name, DELETE | FILE_READ_ATTRIBUTES)?;

    let information = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };

    let size = u32::try_from(size_of::<FILE_DISPOSITION_INFO_EX>()).map_err(io::Error::other)?;

    // SAFETY: the typed input and byte count agree, and file owns the target
    // handle. POSIX disposition removes this name even if another handle still
    // exists; caller-owned shard protection remains the reclamation authority.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const information).cast(),
            size,
        )
    };

    if result == 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

fn nt_result(status: i32) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }

    // SAFETY: this pure status-code translation has no pointer/lifetime inputs.
    let error = unsafe { RtlNtStatusToDosError(status) };

    Err(io::Error::from_raw_os_error(i32::from_ne_bytes(
        error.to_ne_bytes(),
    )))
}

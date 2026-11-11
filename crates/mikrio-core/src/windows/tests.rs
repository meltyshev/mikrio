use super::*;
use crate::test_directory::TestDirectory;
use std::io::{Read as _, Write as _};
use std::os::windows::fs::{symlink_dir, symlink_file};
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};

#[test]
fn relative_operations_retain_directory_identity_after_path_replacement() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let path = temporary.path().join("original");
    let moved = temporary.path().join("moved");
    std::fs::create_dir(&path)?;
    let directory = open_directory(&path)?;
    std::fs::rename(&path, &moved)?;
    std::fs::create_dir(&path)?;

    let mut source = create_file(&directory, "source")?;
    source.write_all(b"original directory")?;
    source.sync_all()?;
    let original = identity(&source)?;
    rename_new(&directory, "source", &directory, "published")?;
    directory.sync_all()?;

    assert!(!path.join("published").exists());
    assert_eq!(path_file_identity(&moved.join("published"))?, original);

    assert_eq!(
        std::fs::read(moved.join("published"))?,
        b"original directory"
    );

    unlink(&directory, "published")?;
    directory.sync_all()?;
    assert!(!moved.join("published").exists());
    drop(source);
    drop(directory);
    temporary.finish()
}

#[test]
fn reparse_entries_and_multiply_linked_identity_preserve_external_sentinels() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let root = temporary.path();
    let outside = root.join("outside");
    std::fs::create_dir(&outside)?;
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"external sentinel")?;
    symlink_dir(&outside, root.join("directory-link"))?;
    symlink_file(&sentinel, root.join("file-link"))?;
    std::fs::hard_link(&sentinel, root.join("hard-link"))?;
    let directory = open_directory(root)?;

    assert_eq!(
        open_child(&directory, "directory-link").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        open_file(&directory, "file-link").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        unlink(&directory, "file-link").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        path_file_identity(&root.join("file-link"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    assert_eq!(
        path_file_identity(&root.join("hard-link"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    // Payload GC removes an owned name, never the other hard link's bytes.
    let payload = open_file(&directory, "hard-link")?;
    unlink(&directory, "hard-link")?;
    assert!(!root.join("hard-link").exists());

    assert_eq!(
        payload.metadata()?.len(),
        u64::try_from(b"external sentinel".len()).unwrap()
    );

    drop(payload);

    assert_eq!(std::fs::read(&sentinel)?, b"external sentinel");
    drop(directory);
    temporary.finish()
}

#[test]
fn writable_directory_flushes_and_read_only_flush_reports_access_denied() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let directory = open_directory(temporary.path())?;
    let file = create_file(&directory, "owned")?;
    file.sync_all()?;
    directory.sync_all()?;

    let read_only = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(temporary.path())?;

    assert_eq!(
        read_only.sync_all().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );

    drop(read_only);
    drop(file);
    drop(directory);
    temporary.finish()
}

#[test]
fn opened_handles_do_not_enable_process_inheritance() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let directory = open_directory(temporary.path())?;
    let file = create_file(&directory, "owned")?;

    for handle in [&directory, &file] {
        let mut flags = 0;

        // SAFETY: handle is live and flags is a correctly sized writable output.
        let result = unsafe { GetHandleInformation(handle.as_raw_handle(), &raw mut flags) };
        assert_ne!(result, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
    }

    drop(file);
    drop(directory);
    temporary.finish()
}

#[test]
fn unlink_removes_the_name_while_an_open_reader_retains_its_bytes() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let directory = open_directory(temporary.path())?;
    create_file(&directory, "owned")?.write_all(b"old bytes")?;
    let mut reader = open_file(&directory, "owned")?;
    unlink(&directory, "owned")?;
    directory.sync_all()?;
    assert!(!temporary.path().join("owned").exists());
    create_file(&directory, "owned")?.write_all(b"new bytes")?;
    let mut original = Vec::new();
    reader.read_to_end(&mut original)?;
    assert_eq!(original, b"old bytes");
    assert_eq!(std::fs::read(temporary.path().join("owned"))?, b"new bytes");
    drop(reader);
    drop(directory);
    temporary.finish()
}

#[test]
fn sharing_conflicts_preserve_names_and_settle_after_the_blocker_closes() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let directory = open_directory(temporary.path())?;
    create_file(&directory, "owned")?.write_all(b"complete payload")?;

    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(temporary.path().join("owned"))?;

    assert_eq!(
        rename_new(&directory, "owned", &directory, "final")
            .unwrap_err()
            .raw_os_error(),
        Some(32)
    );

    assert_eq!(
        unlink(&directory, "owned").unwrap_err().raw_os_error(),
        Some(32)
    );

    assert!(!temporary.path().join("final").exists());

    assert_eq!(
        std::fs::read(temporary.path().join("owned"))?,
        b"complete payload"
    );

    drop(blocker);
    rename_new(&directory, "owned", &directory, "final")?;
    directory.sync_all()?;

    assert_eq!(
        std::fs::read(temporary.path().join("final"))?,
        b"complete payload"
    );

    drop(directory);
    temporary.finish()
}

#[test]
fn relative_names_reject_traversal_and_alternate_streams() -> io::Result<()> {
    let temporary = TestDirectory::new()?;
    let directory = open_directory(temporary.path())?;

    for name in [
        "",
        ".",
        "..",
        "../outside",
        "a\\b",
        "owned:stream",
        "owned.",
        "owned ",
        "a\0b",
    ] {
        assert_eq!(
            create_file(&directory, name).unwrap_err().kind(),
            io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }

    drop(directory);
    temporary.finish()
}

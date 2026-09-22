use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityState, ItemKind, PageRequest, ProviderId, Store,
    StoreError, TotalHint,
};
use musheen_desktop::{
    ArchiveError, ArchiveFormat, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    ArchivePath, ArchiveStore, PasswordRequest,
};
use std::fs::File;
use std::io::{Cursor, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tempfile::tempdir;
use zip::write::SimpleFileOptions;

#[derive(Default)]
struct RecordingPasswords {
    calls: AtomicUsize,
}

impl ArchivePasswordProvider for RecordingPasswords {
    fn request_password(
        &self,
        _request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(Some(ArchivePassword::new(b"correct horse".to_vec())))
    }
}

fn zip_fixture(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, contents) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .expect("fixture entry starts");
        writer.write_all(contents).expect("fixture entry writes");
    }
    writer.finish().expect("fixture closes").into_inner()
}

fn open_bytes(
    bytes: &[u8],
    format: ArchiveFormat,
    limits: ArchiveLimits,
    passwords: Arc<dyn ArchivePasswordProvider>,
) -> ArchiveStore {
    let directory = tempdir().expect("temporary archive directory");
    let path = directory.path().join("fixture.archive");
    std::fs::write(&path, bytes).expect("fixture writes");
    let file = File::open(path).expect("fixture opens");
    ArchiveStore::from_file(file, "fixture.archive", format, passwords, limits)
        .expect("archive store opens")
}

fn test_provider() -> ProviderId {
    ProviderId::new("archive-test").expect("test provider ID")
}

fn decode_hex(hex: &str) -> Vec<u8> {
    hex.trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn read_root(
    store: &ArchiveStore,
    page_size: usize,
) -> Result<Vec<musheen_core::StoreItem>, StoreError> {
    let request = PageRequest::new(page_size, None).expect("valid page size");
    block_on(store.read_directory(&store.root_path(), request, CancellationToken::new()))
        .map(musheen_core::Page::into_items)
}

#[test]
fn archive_paths_reject_platform_escapes_and_nul() {
    for unsafe_name in [
        b"/etc/passwd".as_slice(),
        b"../escape",
        b"a/../../escape",
        br"C:\Windows\system.ini",
        br"./C:\Windows\system.ini",
        br"\\server\share\file",
        b"safe\0hidden",
    ] {
        assert!(
            ArchivePath::new(test_provider(), unsafe_name).is_err(),
            "accepted {unsafe_name:?}"
        );
    }

    assert_eq!(
        ArchivePath::new(test_provider(), b"a/./b//c")
            .expect("safe path")
            .as_bytes(),
        b"a/b/c"
    );

    let other_provider = ProviderId::new("other-archive").expect("other provider ID");
    let first = ArchivePath::new(test_provider(), b"same.txt").expect("first path");
    let second = ArchivePath::new(other_provider.clone(), b"same.txt").expect("second path");
    assert_ne!(first, second);
    assert_eq!(second.provider_id(), &other_provider);
}

#[test]
fn archive_path_enforces_the_4096_byte_boundary() {
    let maximum = vec![b'a'; 4_096];
    assert!(ArchivePath::new(test_provider(), &maximum).is_ok());
    let too_long = vec![b'a'; 4_097];
    assert!(matches!(
        ArchivePath::new(test_provider(), &too_long),
        Err(ArchiveError::LimitExceeded {
            resource: "path bytes",
            ..
        })
    ));
}

#[test]
fn zip_browse_is_lazy_paged_read_only_and_preserves_non_utf8_names() {
    let mut bytes = zip_fixture(&[("alpha.txt", b"a"), ("dir/bravo.txt", b"bb")]);
    let needle = b"alpha.txt";
    let replacement = b"alph\xFF.txt";
    for index in 0..=bytes.len() - needle.len() {
        if &bytes[index..index + needle.len()] == needle {
            bytes[index..index + needle.len()].copy_from_slice(replacement);
        }
    }
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );

    let first_request = PageRequest::new(1, None).expect("page request");
    let first =
        block_on(store.read_directory(&store.root_path(), first_request, CancellationToken::new()))
            .expect("root page");
    assert_eq!(first.items().len(), 1);
    assert_eq!(first.total_hint(), TotalHint::AtLeast(1));
    let first_page_bytes = store.counters().metadata_bytes;
    let second = block_on(store.read_directory(
        &store.root_path(),
        first.next_request().expect("continuation"),
        CancellationToken::new(),
    ))
    .expect("second root page");
    assert_eq!(second.items().len(), 1);
    assert!(store.counters().metadata_bytes > first_page_bytes);
    assert!(first.items().iter().chain(second.items()).any(|item| {
        item.path()
            .provider_key()
            .expect("archive path")
            .1
            .contains(&0xff)
    }));

    for capability in CapabilityKind::ALL
        .into_iter()
        .filter(|kind| *kind != CapabilityKind::CaseSensitivity)
    {
        assert!(matches!(
            store.capabilities(&store.root_path()).get(capability),
            CapabilityState::Unsupported(_)
        ));
    }
    assert_eq!(
        store
            .capabilities(&store.root_path())
            .get(CapabilityKind::CaseSensitivity),
        &CapabilityState::Supported
    );
    assert!(
        store
            .location_writable(&store.root_path())
            .unwrap()
            .reason()
            .is_some()
    );
}

#[test]
fn duplicate_normalized_names_are_rejected() {
    let bytes = zip_fixture(&[("same.txt", b"first"), ("./same.txt", b"second")]);
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let error = read_root(&store, 10).expect_err("duplicates must be ambiguous");
    assert!(error.to_string().contains("duplicate archive path"));
}

#[test]
fn archive_store_rejects_foreign_paths_and_reports_cancellation() {
    let bytes = zip_fixture(&[("one.txt", b"one")]);
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let foreign = ArchivePath::new(
        ProviderId::new("foreign-archive").expect("foreign provider ID"),
        b"one.txt",
    )
    .expect("foreign path")
    .to_store_path()
    .expect("foreign store path");
    let request = PageRequest::new(1, None).expect("page request");
    let error = block_on(store.read_directory(&foreign, request.clone(), CancellationToken::new()))
        .expect_err("foreign paths must not cross archive providers");
    assert!(error.to_string().contains("another provider"));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = block_on(store.read_directory(&store.root_path(), request, cancellation))
        .expect_err("cancelled archive reads must stop");
    assert_eq!(error, StoreError::Cancelled);

    let item = read_root(&store, 1)
        .expect("archive root reads")
        .into_iter()
        .next()
        .expect("archive entry");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = store
        .open_nested(item.path(), ArchiveFormat::Zip, cancellation)
        .expect_err("cancelled nested opens must stop");
    assert_eq!(error, ArchiveError::Cancelled);
}

#[test]
fn tar_links_are_metadata_and_are_never_followed() {
    let directory = tempdir().expect("temporary fixture directory");
    let path = directory.path().join("links.tar");
    let file = File::create(&path).expect("tar fixture creates");
    let mut builder = tar::Builder::new(file);

    let mut regular = tar::Header::new_gnu();
    regular.set_size(4);
    regular.set_mode(0o644);
    regular.set_cksum();
    builder
        .append_data(&mut regular, "target", b"data".as_slice())
        .expect("regular fixture entry");

    let mut symlink = tar::Header::new_gnu();
    symlink.set_entry_type(tar::EntryType::Symlink);
    symlink.set_size(0);
    symlink.set_mode(0o777);
    symlink.set_link_name("target").expect("symlink target");
    symlink.set_cksum();
    builder
        .append_data(&mut symlink, "soft", std::io::empty())
        .expect("symlink fixture entry");

    let mut hardlink = tar::Header::new_gnu();
    hardlink.set_entry_type(tar::EntryType::Link);
    hardlink.set_size(0);
    hardlink.set_mode(0o644);
    hardlink.set_link_name("target").expect("hardlink target");
    hardlink.set_cksum();
    builder
        .append_data(&mut hardlink, "hard", std::io::empty())
        .expect("hardlink fixture entry");
    builder.finish().expect("tar fixture closes");

    let store = ArchiveStore::from_file(
        File::open(path).expect("fixture opens"),
        "links.tar",
        ArchiveFormat::Tar,
        Arc::new(RecordingPasswords::default()),
        ArchiveLimits::default(),
    )
    .expect("archive store opens");
    let items = read_root(&store, 10).expect("tar root reads");
    assert!(
        items
            .iter()
            .any(|item| item.kind() == ItemKind::SymbolicLink)
    );
    assert!(items.iter().any(|item| item.kind() == ItemKind::Other));
}

#[test]
fn encrypted_zip_metadata_does_not_request_an_unneeded_secret() {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "correct horse");
    writer.start_file("private-name.txt", options).unwrap();
    writer.write_all(b"secret").unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let passwords = Arc::new(RecordingPasswords::default());
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        passwords.clone(),
    );
    let items = read_root(&store, 10).expect("encrypted metadata reads");
    assert_eq!(items.len(), 1);
    assert_eq!(passwords.calls.load(Ordering::Relaxed), 0);
    assert!(!format!("{store:?}").contains("correct horse"));
}

#[test]
fn encrypted_seven_zip_header_uses_the_password_callback() {
    use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options};
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, Password};

    assert_eq!(
        format!("{:?}", Password::new("do not log me")),
        "Password([REDACTED])"
    );

    let mut bytes = Vec::new();
    {
        let mut writer = ArchiveWriter::new(Cursor::new(&mut bytes)).expect("7z writer starts");
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::new("correct horse")).into(),
            Lzma2Options::default().into(),
        ]);
        writer
            .push_archive_entry(
                ArchiveEntry::new_file("encrypted-name.txt"),
                Some(b"content".as_slice()),
            )
            .expect("encrypted entry writes");
        writer.finish().expect("7z writer closes");
    }
    let passwords = Arc::new(RecordingPasswords::default());
    let store = open_bytes(
        &bytes,
        ArchiveFormat::SevenZip,
        ArchiveLimits::default(),
        passwords.clone(),
    );
    let items = read_root(&store, 10).expect("encrypted header reads");
    assert_eq!(items.len(), 1);
    assert_eq!(passwords.calls.load(Ordering::Relaxed), 1);
    assert!(!format!("{store:?}").contains("correct horse"));

    let wrong_passwords =
        Arc::new(|_: &PasswordRequest| Ok(Some(ArchivePassword::new(b"wrong guess".to_vec()))));
    let store = open_bytes(
        &bytes,
        ArchiveFormat::SevenZip,
        ArchiveLimits::default(),
        wrong_passwords,
    );
    let error = read_root(&store, 10).expect_err("wrong passwords must fail closed");
    let message = error.to_string();
    assert!(!message.contains("wrong guess"));
    assert!(!message.contains("encrypted-name.txt"));
}

#[test]
fn seven_zip_metadata_budget_is_charged_before_header_allocations() {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

    let mut bytes = Vec::new();
    {
        let mut writer = ArchiveWriter::new(Cursor::new(&mut bytes)).expect("7z writer starts");
        for index in 0..128 {
            writer
                .push_archive_entry(
                    ArchiveEntry::new_directory(&format!("directory-{index:03}")),
                    None::<std::io::Empty>,
                )
                .expect("7z directory writes");
        }
        writer.finish().expect("7z writer closes");
    }
    let limit = 4_096;
    let store = open_bytes(
        &bytes,
        ArchiveFormat::SevenZip,
        ArchiveLimits {
            max_metadata_bytes: limit,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let error = read_root(&store, 256).expect_err("7z metadata must respect the memory budget");
    assert!(error.to_string().contains("metadata bytes limit exceeded"));
    let counters = store.counters();
    assert!(counters.total_allocated_bytes > 0);
    assert!(counters.peak_metadata_bytes > 0);
    assert!(counters.peak_metadata_bytes <= limit);
    assert_eq!(counters.metadata_bytes, 0);
}

#[test]
fn compressed_tar_variants_browse_without_extraction() {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(7);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "folder/file.txt", b"content".as_slice())
            .expect("tar entry writes");
        builder.finish().expect("tar closes");
    }

    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar_bytes).unwrap();
    let gzip = gzip.finish().unwrap();
    let zstd = zstd::stream::encode_all(tar_bytes.as_slice(), 1).unwrap();

    for (format, bytes) in [
        (ArchiveFormat::TarGzip, gzip.as_slice()),
        (ArchiveFormat::TarZstd, zstd.as_slice()),
    ] {
        let store = open_bytes(
            bytes,
            format,
            ArchiveLimits::default(),
            Arc::new(RecordingPasswords::default()),
        );
        let root = read_root(&store, 10).expect("compressed tar root reads");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].kind(), ItemKind::Directory);
    }
}

#[test]
fn malformed_metadata_is_bounded_by_time_and_allocation_counters() {
    for (name, hex) in [
        (
            "later central signature",
            include_str!("fixtures/archive/later-central-signature.hex"),
        ),
        (
            "oversized central name",
            include_str!("fixtures/archive/oversized-central-name.hex"),
        ),
        (
            "duplicate normalized name",
            include_str!("fixtures/archive/duplicate-normalized-name.hex"),
        ),
    ] {
        let limits = ArchiveLimits {
            max_metadata_bytes: 256 * 1_024,
            max_elapsed: Duration::from_millis(250),
            ..ArchiveLimits::default()
        };
        let store = open_bytes(
            &decode_hex(hex),
            ArchiveFormat::Zip,
            limits,
            Arc::new(RecordingPasswords::default()),
        );
        let started = Instant::now();
        let error = read_root(&store, 10).expect_err(name);
        assert!(started.elapsed() < Duration::from_secs(1), "{name}");
        assert!(!error.to_string().contains("private-name"), "{name}");
        let counters = store.counters();
        assert!(counters.total_allocated_bytes > 0, "{name}");
        assert!(counters.peak_metadata_bytes > 0, "{name}");
        assert!(counters.metadata_bytes <= 256 * 1_024, "{name}");
        assert!(counters.elapsed <= Duration::from_secs(1), "{name}");
    }
}

#[test]
fn metadata_allocation_limit_trips_after_index_work() {
    let directory = tempdir().expect("temporary fixture directory");
    let path = directory.path().join("allocation-limit.tar");
    let file = File::create(&path).expect("tar fixture creates");
    let mut builder = tar::Builder::new(file);
    for name in ["first", "a/second/path/with/a/longer/name"] {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, b"x".as_slice())
            .expect("tar entry writes");
    }
    builder.finish().expect("tar fixture closes");

    let limit = 300;
    let store = ArchiveStore::from_file(
        File::open(path).expect("fixture opens"),
        "allocation-limit.tar",
        ArchiveFormat::Tar,
        Arc::new(RecordingPasswords::default()),
        ArchiveLimits {
            max_metadata_bytes: limit,
            ..ArchiveLimits::default()
        },
    )
    .expect("archive store opens");
    let error = read_root(&store, 10).expect_err("metadata allocation limit must fail closed");
    assert!(error.to_string().contains("metadata bytes limit exceeded"));
    let counters = store.counters();
    assert!(counters.total_allocated_bytes > 0);
    assert!(counters.peak_metadata_bytes <= limit);
}

#[test]
fn nested_archive_depth_is_limited_to_eight() {
    let mut bytes = zip_fixture(&[("leaf.txt", b"leaf")]);
    for _ in 0..9 {
        bytes = zip_fixture(&[("inner.zip", bytes.as_slice())]);
    }
    let mut store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    for expected_depth in 1..=8 {
        let item = read_root(&store, 10)
            .expect("nested directory reads")
            .into_iter()
            .next()
            .expect("nested archive entry");
        store = store
            .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
            .expect("nested archive opens within the limit");
        assert_eq!(store.nesting_depth(), expected_depth);
    }
    let item = read_root(&store, 10)
        .expect("eighth nested directory reads")
        .into_iter()
        .next()
        .expect("ninth nested archive entry");
    let error = store
        .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect_err("ninth nested archive is rejected");
    assert!(matches!(
        error,
        ArchiveError::LimitExceeded {
            resource: "nested archives",
            ..
        }
    ));
}

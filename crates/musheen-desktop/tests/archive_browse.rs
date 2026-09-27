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
use std::sync::Barrier;
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

fn compressed_zip_fixture(name: &str, contents: &[u8]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            name,
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .expect("fixture entry starts");
    writer.write_all(contents).expect("fixture entry writes");
    writer.finish().expect("fixture closes").into_inner()
}

fn zip64_size_fixture(name: &str, declared_size: u64) -> Vec<u8> {
    let mut bytes = zip_fixture(&[(name, b"x")]);
    let central = bytes
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("central header");
    let eocd = bytes
        .windows(4)
        .position(|window| window == b"PK\x05\x06")
        .expect("end record");
    bytes[central + 20..central + 28].fill(0xff);
    bytes[central + 30..central + 32].copy_from_slice(&20_u16.to_le_bytes());
    let zip64_extra = [
        0x01,
        0x00,
        0x10,
        0x00,
        declared_size.to_le_bytes()[0],
        declared_size.to_le_bytes()[1],
        declared_size.to_le_bytes()[2],
        declared_size.to_le_bytes()[3],
        declared_size.to_le_bytes()[4],
        declared_size.to_le_bytes()[5],
        declared_size.to_le_bytes()[6],
        declared_size.to_le_bytes()[7],
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    bytes.splice(eocd..eocd, zip64_extra);
    let new_eocd = eocd + 20;
    let central_size = u32::from_le_bytes(
        bytes[new_eocd + 12..new_eocd + 16]
            .try_into()
            .expect("central size"),
    ) + 20;
    bytes[new_eocd + 12..new_eocd + 16].copy_from_slice(&central_size.to_le_bytes());
    bytes
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

#[cfg(feature = "archive-libarchive")]
fn cancel_large_read(store: &ArchiveStore) -> (StoreError, Duration) {
    let cancellation = CancellationToken::new();
    let cancel_from_thread = cancellation.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1));
        cancel_from_thread.cancel();
    });
    let started = Instant::now();
    let result = block_on(store.read_directory(
        &store.root_path(),
        PageRequest::new(2_049, None).expect("large page"),
        cancellation,
    ));
    cancel.join().expect("cancellation thread");
    (
        result.expect_err("worker scan must observe cancellation"),
        started.elapsed(),
    )
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

    let retry = read_root(&store, 10).expect_err("a consumed scan failure must stay terminal");
    assert_eq!(retry.to_string(), error.to_string());
}

#[test]
fn file_directory_namespace_collisions_are_rejected_in_both_orders() {
    for entries in [
        [("a", b"file".as_slice()), ("a/b", b"child".as_slice())],
        [("a/b", b"child".as_slice()), ("a", b"file".as_slice())],
    ] {
        let store = open_bytes(
            &zip_fixture(&entries),
            ArchiveFormat::Zip,
            ArchiveLimits::default(),
            Arc::new(RecordingPasswords::default()),
        );
        let error = read_root(&store, 10).expect_err("file/directory collision must fail closed");
        assert!(error.to_string().contains("duplicate archive path"));
    }
}

#[test]
fn zip64_entry_sizes_are_preserved_and_large_declarations_are_bounded() {
    let five_gib = 5_u64 * 1_024 * 1_024 * 1_024;
    let store = open_bytes(
        &zip64_size_fixture("large.bin", five_gib),
        ArchiveFormat::Zip,
        ArchiveLimits {
            max_expanded_bytes: 6_u64 * 1_024 * 1_024 * 1_024,
            max_compression_ratio: u64::MAX,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let item = read_root(&store, 1)
        .expect("ZIP64 metadata reads")
        .into_iter()
        .next()
        .expect("ZIP64 entry");
    assert_eq!(item.size(), Some(five_gib));

    let mut header = tar::Header::new_gnu();
    header.set_path("huge.bin").expect("tar path");
    header.set_size(21_u64 * 1_024 * 1_024 * 1_024);
    header.set_mode(0o644);
    header.set_cksum();
    let mut tar = header.as_bytes().to_vec();
    tar.extend_from_slice(&[0_u8; 1_024]);
    let store = open_bytes(
        &tar,
        ArchiveFormat::Tar,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let error = read_root(&store, 1).expect_err("20 GiB expanded limit must apply to browsing");
    assert!(error.to_string().contains("expanded bytes limit exceeded"));
}

#[test]
fn compression_ratio_limit_applies_while_browsing() {
    let bytes = compressed_zip_fixture("bomb.bin", &vec![0_u8; 1024 * 1024]);
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Zip,
        ArchiveLimits {
            max_compression_ratio: 2,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let error = read_root(&store, 1).expect_err("compression ratio must be bounded");
    assert!(
        error
            .to_string()
            .contains("compression ratio limit exceeded")
    );
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
fn encrypted_nested_zip_uses_the_bounded_single_entry_reader() {
    let inner = zip_fixture(&[("inside.txt", b"inside")]);
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        SimpleFileOptions::default().with_aes_encryption(zip::AesMode::Aes256, "correct horse");
    writer
        .start_file("inner.zip", options)
        .expect("AES entry starts");
    writer.write_all(&inner).expect("AES entry writes");
    let outer = writer.finish().expect("AES archive closes").into_inner();
    let passwords = Arc::new(RecordingPasswords::default());
    let store = open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        passwords.clone(),
    );
    let item = read_root(&store, 1)
        .expect("encrypted metadata reads without a secret")
        .remove(0);
    let child = store
        .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect("encrypted nested ZIP opens through the bounded target reader");
    assert_eq!(passwords.calls.load(Ordering::Relaxed), 1);
    assert_eq!(read_root(&child, 1).expect("nested content reads").len(), 1);
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

#[test]
fn nested_open_keeps_live_scanner_position_and_stable_identity() {
    let inner = zip_fixture(&[("leaf.txt", b"leaf")]);
    let outer = zip_fixture(&[("inner.zip", inner.as_slice()), ("after.txt", b"after")]);
    let store = open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let first_request = PageRequest::new(1, None).expect("first request");
    let first =
        block_on(store.read_directory(&store.root_path(), first_request, CancellationToken::new()))
            .expect("first page");
    let nested_path = first.items()[0].path().clone();
    let continuation = first.next_request().expect("second request");

    let child_a = store
        .open_nested(&nested_path, ArchiveFormat::Zip, CancellationToken::new())
        .expect("first nested open");
    let child_b = store
        .open_nested(&nested_path, ArchiveFormat::Zip, CancellationToken::new())
        .expect("second nested open");
    assert_eq!(child_a.provider_id(), child_b.provider_id());

    let second =
        block_on(store.read_directory(&store.root_path(), continuation, CancellationToken::new()))
            .expect("live scanner keeps its independent cursor");
    assert_eq!(second.items().len(), 1);
    assert_eq!(second.items()[0].display_name().as_str(), "after.txt");
}

#[test]
fn nested_open_and_paging_can_run_concurrently() {
    let payload = (0..4 * 1_024 * 1_024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let inner = zip_fixture(&[("payload.bin", payload.as_slice())]);
    let outer = zip_fixture(&[("inner.zip", inner.as_slice()), ("after.txt", b"after")]);
    let store = Arc::new(open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    ));
    let first = block_on(store.read_directory(
        &store.root_path(),
        PageRequest::new(1, None).expect("first request"),
        CancellationToken::new(),
    ))
    .expect("first page");
    let nested_path = first.items()[0].path().clone();
    let continuation = first.next_request().expect("continuation");
    let barrier = Arc::new(Barrier::new(3));

    std::thread::scope(|scope| {
        let nested_store = Arc::clone(&store);
        let nested_barrier = Arc::clone(&barrier);
        let nested = scope.spawn(move || {
            nested_barrier.wait();
            nested_store.open_nested(&nested_path, ArchiveFormat::Zip, CancellationToken::new())
        });
        let paging_store = Arc::clone(&store);
        let paging_barrier = Arc::clone(&barrier);
        let page = scope.spawn(move || {
            paging_barrier.wait();
            block_on(paging_store.read_directory(
                &paging_store.root_path(),
                continuation,
                CancellationToken::new(),
            ))
        });
        barrier.wait();
        nested.join().expect("nested thread").expect("nested open");
        let page = page.join().expect("paging thread").expect("second page");
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].display_name().as_str(), "after.txt");
    });
}

#[test]
fn nested_byte_limit_preserves_the_typed_limit_error() {
    let inner = zip_fixture(&[("leaf.txt", &[b'x'; 256])]);
    let outer = zip_fixture(&[("inner.zip", inner.as_slice())]);
    let store = open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits {
            max_nested_archive_bytes: 32,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let item = read_root(&store, 1)
        .expect("outer archive reads")
        .into_iter()
        .next()
        .expect("nested entry");
    let error = store
        .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect_err("nested bytes must be bounded");
    assert!(matches!(
        error,
        ArchiveError::LimitExceeded {
            resource: "nested archive bytes",
            ..
        }
    ));
}

#[test]
fn nested_deflated_zip_uses_the_budgeted_target_reader() {
    let inner = zip_fixture(&[("leaf.txt", b"leaf")]);
    let outer = compressed_zip_fixture("inner.zip", &inner);
    let store = open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let item = read_root(&store, 1).expect("outer archive reads").remove(0);
    let child = store
        .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect("deflated nested ZIP opens without an archive-wide metadata index");
    assert_eq!(read_root(&child, 1).expect("nested ZIP reads").len(), 1);
}

#[test]
fn nested_zip_cp437_names_reserve_raw_and_decoded_metadata_before_allocation() {
    let inner = zip_fixture(&[("inside.txt", b"inside")]);
    let long_name = format!("{}.zip", "a".repeat(4_091));
    let mut outer = zip_fixture(&[(&long_name, &inner)]);
    let local = outer
        .windows(4)
        .position(|window| window == b"PK\x03\x04")
        .expect("local header");
    let central = outer
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("central header");
    outer[local + 30] = 0x82;
    outer[central + 46] = 0x82;
    let store = open_bytes(
        &outer,
        ArchiveFormat::Zip,
        ArchiveLimits {
            max_metadata_bytes: 28 * 1_024,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let item = read_root(&store, 1)
        .expect("the incrementally indexed outer entry fits")
        .remove(0);
    let error = store
        .open_nested(item.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect_err("raw plus decoded CP437 metadata must be reserved before allocation");
    assert!(matches!(
        error,
        ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            ..
        }
    ));
    assert!(store.counters().peak_metadata_bytes <= 28 * 1_024);
}

#[test]
fn nested_seven_zip_uses_budgeted_sequential_decode() {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};

    let inner = zip_fixture(&[("leaf.txt", b"leaf")]);
    let mut outer = Vec::new();
    {
        let mut writer = ArchiveWriter::new(Cursor::new(&mut outer)).expect("7z writer starts");
        writer
            .push_archive_entry(
                ArchiveEntry::new_file("decoy.bin"),
                Some(vec![0_u8; 128 * 1024].as_slice()),
            )
            .expect("decoy writes");
        writer
            .push_archive_entry(ArchiveEntry::new_file("inner.zip"), Some(inner.as_slice()))
            .expect("nested archive writes");
        writer.finish().expect("7z writer closes");
    }
    let store = open_bytes(
        &outer,
        ArchiveFormat::SevenZip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let nested = read_root(&store, 10)
        .expect("7z root reads")
        .into_iter()
        .find(|item| item.display_name().as_str() == "inner.zip")
        .expect("nested entry");
    let retained_before = store.counters().metadata_bytes;
    let child = store
        .open_nested(nested.path(), ArchiveFormat::Zip, CancellationToken::new())
        .expect("nested 7z entry opens");
    assert_eq!(read_root(&child, 1).expect("inner ZIP reads").len(), 1);
    assert_eq!(store.counters().metadata_bytes, retained_before);
    assert!(store.counters().peak_metadata_bytes <= ArchiveLimits::default().max_metadata_bytes);
}

#[test]
fn compressed_tar_skip_observes_mid_stream_cancellation() {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let payload = vec![0_u8; 32 * 1_024 * 1_024];
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "large.bin", payload.as_slice())
            .expect("tar entry writes");
        builder.finish().expect("tar closes");
    }
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar_bytes).expect("gzip writes");
    let gzip = gzip.finish().expect("gzip closes");
    let store = open_bytes(
        &gzip,
        ArchiveFormat::TarGzip,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let cancellation = CancellationToken::new();
    let cancel_from_thread = cancellation.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1));
        cancel_from_thread.cancel();
    });
    let error = block_on(store.read_directory(
        &store.root_path(),
        PageRequest::new(2, None).expect("page request"),
        cancellation,
    ))
    .expect_err("compressed tar decoding must stop after cancellation");
    cancel.join().expect("cancel thread");
    assert_eq!(error, StoreError::Cancelled);
    assert_eq!(
        read_root(&store, 2)
            .expect("fresh token retries from a rebuilt compressed-tar scanner")
            .len(),
        1
    );
}

#[test]
fn compressed_tar_stream_enforces_expanded_limit_before_an_entry() {
    let extension_size = 256 * 1_024_u64;
    let mut extension = tar::Header::new_gnu();
    extension.set_entry_type(tar::EntryType::GNULongName);
    extension.set_size(extension_size);
    extension.set_mode(0o644);
    extension.set_cksum();
    let mut tar_bytes = extension.as_bytes().to_vec();
    tar_bytes.resize(512 + extension_size as usize, b'a');
    tar_bytes.extend_from_slice(&[0_u8; 1_024]);

    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar_bytes).expect("gzip writes");
    let gzip = gzip.finish().expect("gzip closes");
    let store = open_bytes(
        &gzip,
        ArchiveFormat::TarGzip,
        ArchiveLimits {
            max_expanded_bytes: 64 * 1_024,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let error = read_root(&store, 1).expect_err("stream expansion must be bounded while scanning");
    assert!(error.to_string().contains("expanded bytes limit exceeded"));
}

#[cfg(feature = "archive-libarchive")]
#[test]
fn libarchive_scanner_advances_linearly() {
    let entries = (0..64)
        .map(|index| (format!("entry-{index:03}.txt"), vec![b'x']))
        .collect::<Vec<_>>();
    let borrowed = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect::<Vec<_>>();
    let bytes = zip_fixture(&borrowed);
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Iso,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let mut request = PageRequest::new(1, None).expect("first request");
    let mut midpoint = 0;
    for index in 0..32 {
        let page =
            block_on(store.read_directory(&store.root_path(), request, CancellationToken::new()))
                .expect("libarchive page");
        request = page.next_request().expect("next page");
        if index == 15 {
            midpoint = store.counters().bytes_read;
        }
    }
    let total = store.counters().bytes_read;
    assert!(
        total.saturating_sub(midpoint) <= midpoint,
        "second half reread more bytes than the first: midpoint={midpoint}, total={total}"
    );
}

#[cfg(feature = "archive-libarchive")]
#[test]
fn libarchive_worker_enforces_expansion_and_timeout_bounds() {
    let bytes = zip_fixture(&[("entry.txt", b"contents")]);
    let expansion_store = open_bytes(
        &bytes,
        ArchiveFormat::Iso,
        ArchiveLimits {
            max_expanded_bytes: 1,
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let expansion = read_root(&expansion_store, 1).expect_err("worker byte bound must trip");
    assert!(
        expansion
            .to_string()
            .contains("expanded bytes limit exceeded"),
        "unexpected expansion error: {expansion}"
    );

    let timeout_store = open_bytes(
        &bytes,
        ArchiveFormat::Iso,
        ArchiveLimits {
            max_elapsed: Duration::from_nanos(1),
            ..ArchiveLimits::default()
        },
        Arc::new(RecordingPasswords::default()),
    );
    let timeout = read_root(&timeout_store, 1).expect_err("worker deadline must trip");
    assert!(timeout.to_string().contains("milliseconds limit exceeded"));
}

#[cfg(feature = "archive-libarchive")]
#[test]
fn libarchive_worker_cancellation_retries_and_drop_is_bounded() {
    let entries = (0..2_048)
        .map(|index| {
            (
                format!("entry-{index:04}-{}.txt", "x".repeat(128)),
                vec![b'x'],
            )
        })
        .collect::<Vec<_>>();
    let borrowed = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect::<Vec<_>>();
    let bytes = zip_fixture(&borrowed);
    let store = open_bytes(
        &bytes,
        ArchiveFormat::Iso,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let (error, cancellation_latency) = cancel_large_read(&store);
    assert_eq!(error, StoreError::Cancelled);
    assert!(
        cancellation_latency < Duration::from_millis(500),
        "a cancelled request must not synchronously rebuild the libarchive scanner"
    );
    let elapsed_after_cancel = store.counters().elapsed;

    let (replay_error, _) = cancel_large_read(&store);
    assert_eq!(replay_error, StoreError::Cancelled);
    assert!(store.counters().elapsed > elapsed_after_cancel);
    assert_eq!(
        read_root(&store, 2_049)
            .expect("a later fresh request replays and completes")
            .len(),
        2_048
    );

    let started = Instant::now();
    drop(store);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "worker teardown must kill and reap without an indefinite join"
    );
}

#[test]
fn nested_open_in_a_tar_after_a_pax_global_header_copies_its_own_entry() {
    // `git archive` starts every tar with a pax global header.
    let inner = zip_fixture(&[("leaf.txt", b"leaf")]);
    let record = format!(" comment={}\n", "0".repeat(40));
    let record = format!("{}{record}", record.len() + 2);
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut global = tar::Header::new_ustar();
        global.set_entry_type(tar::EntryType::XGlobalHeader);
        global
            .set_path("pax_global_header")
            .expect("global header path");
        global.set_size(record.len() as u64);
        global.set_mode(0o666);
        global.set_cksum();
        builder
            .append(&global, record.as_bytes())
            .expect("global header writes");
        for (name, contents) in [("before.txt", &b"before"[..]), ("inner.zip", &inner[..])] {
            let mut header = tar::Header::new_ustar();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, name, contents)
                .expect("tar entry writes");
        }
        builder.finish().expect("tar closes");
    }
    let store = open_bytes(
        &tar_bytes,
        ArchiveFormat::Tar,
        ArchiveLimits::default(),
        Arc::new(RecordingPasswords::default()),
    );
    let root = read_root(&store, 10).expect("tar root reads");
    let nested_path = root
        .iter()
        .find(|item| item.display_name().as_str() == "inner.zip")
        .expect("inner.zip is listed")
        .path()
        .clone();

    let child = store
        .open_nested(&nested_path, ArchiveFormat::Zip, CancellationToken::new())
        .expect("the nested ZIP holds its own bytes");
    let leaves = read_root(&child, 10).expect("nested root reads");
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].display_name().as_str(), "leaf.txt");
}

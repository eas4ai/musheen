use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, SearchCompletion, SearchQuery, Store, StoreError, StorePath,
};
use musheen_local::LocalStore;
use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct SearchOutcome {
    names: Vec<String>,
    scope_errors: usize,
    completion: SearchCompletion,
}

fn collect_search(store: &LocalStore, root: &std::path::Path, expression: &str) -> SearchOutcome {
    let query = SearchQuery::parse(expression).expect("query is valid");
    let cancellation = CancellationToken::new();
    let scope = StorePath::from_unix_path(root.as_os_str().to_os_string());
    let mut stream =
        block_on(store.search(&scope, query, cancellation.clone())).expect("search starts");
    let mut names = Vec::new();
    let mut scope_errors = 0;
    let mut completion = SearchCompletion::Running;
    while let Some(batch) =
        block_on(stream.next_batch(cancellation.clone())).expect("batch is available")
    {
        names.extend(
            batch
                .results()
                .iter()
                .map(|result| result.item().display_name().as_str().to_owned()),
        );
        scope_errors += batch.errors().len();
        completion = batch.completion();
        if completion != SearchCompletion::Running {
            break;
        }
    }
    names.sort();
    SearchOutcome {
        names,
        scope_errors,
        completion,
    }
}

fn collect_names(store: &LocalStore, root: &std::path::Path, expression: &str) -> Vec<String> {
    collect_search(store, root, expression).names
}

#[test]
fn recursive_search_supports_name_content_and_scope() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let nested = temporary.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("report.txt"), b"quarterly needle").unwrap();
    let mut boundary_content = vec![b'x'; 64 * 1_024 - 3];
    boundary_content.extend_from_slice(b"needle");
    fs::write(nested.join("boundary.txt"), boundary_content).unwrap();
    fs::write(temporary.path().join("other.txt"), b"not a match").unwrap();
    let hidden_scope = temporary.path().join(".scope");
    fs::create_dir(&hidden_scope).unwrap();
    fs::write(
        hidden_scope.join("inside.txt"),
        b"visible in explicit scope",
    )
    .unwrap();
    let store = LocalStore::new();

    assert_eq!(
        collect_names(&store, temporary.path(), "name:report"),
        ["report.txt"]
    );
    assert_eq!(
        collect_names(&store, temporary.path(), "content:needle"),
        ["boundary.txt", "report.txt"]
    );
    assert!(collect_names(&store, &nested, "name:other").is_empty());
    assert_eq!(
        collect_names(&store, &hidden_scope, "name:inside"),
        ["inside.txt"]
    );
}

#[test]
fn search_applies_glob_mime_size_date_and_hidden_policy() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    fs::write(temporary.path().join("visible.txt"), b"12345678").unwrap();
    fs::write(temporary.path().join("small.txt"), b"1").unwrap();
    fs::write(temporary.path().join("visible.bin"), b"12345678").unwrap();
    fs::write(temporary.path().join("photo.png"), b"not-real-png").unwrap();
    fs::write(temporary.path().join(".hidden.txt"), b"12345678").unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let store = LocalStore::new();

    let expression = format!(
        "name:visible glob:*.txt mime:text/plain size:>=8 modified:<={} hidden:false",
        now + 2
    );
    assert_eq!(
        collect_names(&store, temporary.path(), &expression),
        ["visible.txt"]
    );
    assert_eq!(
        collect_names(&store, temporary.path(), "name:hidden hidden:true"),
        [".hidden.txt"]
    );
    assert_eq!(
        collect_names(&store, temporary.path(), "type:image"),
        ["photo.png"]
    );
}

#[test]
fn invalid_queries_fail_before_provider_work_starts() {
    for expression in [
        "",
        "unknown:value",
        "size:large",
        "size:9..1",
        "modified:5..2",
        "hidden:perhaps",
    ] {
        assert!(
            SearchQuery::parse(expression).is_err(),
            "accepted {expression}"
        );
    }
}

#[test]
fn cancellation_interrupts_a_stalled_search_consumer() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    for index in 0..3_000 {
        fs::write(temporary.path().join(format!("item-{index}.txt")), b"match").unwrap();
    }
    let store = LocalStore::new();
    let cancellation = CancellationToken::new();
    let scope = StorePath::from_unix_path(temporary.path().as_os_str().to_os_string());
    let mut stream = block_on(store.search(
        &scope,
        SearchQuery::parse("name:item").unwrap(),
        cancellation.clone(),
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    cancellation.cancel();

    assert!(matches!(
        block_on(stream.next_batch(cancellation)),
        Err(StoreError::Cancelled)
    ));
}

#[test]
fn dropping_a_search_stream_cancels_its_worker() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    fs::write(temporary.path().join("item.txt"), b"match").unwrap();
    let cancellation = CancellationToken::new();
    let scope = StorePath::from_unix_path(temporary.path().as_os_str().to_os_string());
    let stream = block_on(LocalStore::new().search(
        &scope,
        SearchQuery::parse("name:item").unwrap(),
        cancellation.clone(),
    ))
    .unwrap();

    drop(stream);

    assert!(cancellation.is_cancelled());
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed_by_default_and_loops_terminate_when_enabled() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let nested = temporary.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("report.txt"), b"report").unwrap();
    symlink(temporary.path(), nested.join("loop")).unwrap();
    let store = LocalStore::new();

    assert_eq!(
        collect_names(&store, temporary.path(), "kind:link"),
        ["loop"]
    );
    let followed = collect_search(&store, temporary.path(), "name:report follow-links:true");
    assert_eq!(followed.names, ["report.txt"]);
    assert!(followed.scope_errors > 0);
    assert_eq!(followed.completion, SearchCompletion::Complete);
}

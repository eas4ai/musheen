use musheen_core::{
    DisplayPath, ItemId, ItemKind, ProviderId, SEARCH_BATCH_ERRORS, SEARCH_QUERY_BYTES,
    SEARCH_QUERY_TERMS, SearchBatch, SearchCapabilities, SearchCompletion, SearchQuery,
    SearchResult, SearchScopeError, StoreItem, StorePath,
};
use musheen_ui::search::{DirectoryFilter, SearchResultModel, SearchState};
use musheen_ui::views::DirectoryViewModel;

fn item(index: u64, name: &str, kind: ItemKind, size: Option<u64>) -> StoreItem {
    let provider = ProviderId::new("local").unwrap();
    StoreItem::new(
        ItemId::new(provider, index.to_be_bytes()).unwrap(),
        StorePath::from_unix_path(format!("/scope/{name}")),
        DisplayPath::new(name),
        kind,
        size,
    )
    .with_modified_unix_seconds(index as i64)
}

fn result(index: u64) -> SearchResult {
    SearchResult::new(
        item(
            index,
            &format!("item-{index}.txt"),
            ItemKind::RegularFile,
            Some(index),
        ),
        Some("text/plain"),
    )
}

#[test]
fn parsed_queries_report_required_provider_metadata() {
    let query = SearchQuery::parse(
        "name:report content:needle mime:text/plain size:>=8 modified:1..9 hidden:true",
    )
    .unwrap();
    let limited = SearchCapabilities::names_only();
    assert!(
        SearchCapabilities::default()
            .validate(&SearchQuery::parse("name:report").unwrap())
            .is_err()
    );

    assert!(
        limited
            .validate(&SearchQuery::parse("name:report").unwrap())
            .is_ok()
    );
    assert!(limited.validate(&query).is_err());
    for expression in ["kind:file", "hidden:true", "follow-links:true"] {
        assert!(
            limited
                .validate(&SearchQuery::parse(expression).unwrap())
                .is_err(),
            "limited provider accepted {expression}"
        );
    }
    assert!(SearchCapabilities::all().validate(&query).is_ok());

    let exact = SearchQuery::parse(r#"name:"quarterly report" size:8"#).unwrap();
    assert_eq!(exact.name_terms()[0].as_ref(), "quarterly report");
    assert!(exact.size().unwrap().matches(&8));
    assert!(!exact.size().unwrap().matches(&9));
    assert!(SearchQuery::parse("hidden:true hidden:false").is_err());
    assert!(SearchQuery::parse(&"x".repeat(SEARCH_QUERY_BYTES + 1)).is_err());
    assert!(SearchQuery::parse(&vec!["x"; SEARCH_QUERY_TERMS + 1].join(" ")).is_err());

    let inherited_hidden = SearchQuery::parse("name:item")
        .unwrap()
        .with_default_hidden_policy(true);
    assert!(inherited_hidden.include_hidden());
    assert!(limited.validate(&inherited_hidden).is_err());
    assert!(
        !SearchQuery::parse("name:item hidden:false")
            .unwrap()
            .with_default_hidden_policy(true)
            .include_hidden()
    );

    let errors = (0..=SEARCH_BATCH_ERRORS)
        .map(|index| {
            SearchScopeError::new(
                StorePath::from_unix_path(format!("/scope/error-{index}")),
                "denied",
                false,
            )
        })
        .collect();
    assert!(SearchBatch::running(vec![], errors).is_err());
}

#[test]
fn stale_batches_are_discarded_and_partial_errors_remain_visible() {
    let scope = StorePath::from_unix_path("/scope");
    let query = SearchQuery::parse("name:item").unwrap();
    let mut model = SearchResultModel::new(scope, query, 4_096, 100_000);
    let first = model.begin();
    let second = model.begin();

    assert!(!model.apply(
        first,
        SearchBatch::running(vec![result(1)], vec![]).unwrap()
    ));
    assert!(
        model.apply(
            second,
            SearchBatch::new(
                vec![result(2)],
                vec![musheen_core::SearchScopeError::new(
                    StorePath::from_unix_path("/scope/denied"),
                    "permission denied",
                    true,
                )],
                SearchCompletion::Complete,
            )
            .unwrap(),
        )
    );
    assert_eq!(
        model.retained_results()[0].item().display_name().as_str(),
        "item-2.txt"
    );
    assert_eq!(model.errors().len(), 1);
    assert_eq!(model.state(), SearchState::Partial);
}

#[test]
fn million_result_producer_requests_refinement_without_unbounded_models() {
    let mut model = SearchResultModel::new(
        StorePath::from_unix_path("/scope"),
        SearchQuery::parse("name:item").unwrap(),
        4_096,
        100_000,
    );
    let generation = model.begin();
    let mut accepted_batches = 0;
    for batch in 0..1_000_000_u64.div_ceil(256) {
        let results = (0..256)
            .map(|offset| result(batch * 256 + offset))
            .collect();
        accepted_batches +=
            usize::from(model.apply(generation, SearchBatch::running(results, vec![]).unwrap()));
    }

    assert_eq!(accepted_batches, 391);
    assert_eq!(model.total_results(), 100_000);
    assert_eq!(model.retained_results().len(), 4_096);
    assert_eq!(model.state(), SearchState::RefineRequired);
}

#[test]
fn in_view_filter_clears_without_reloading_the_directory() {
    let mut directory = DirectoryViewModel::new(4_096);
    directory.extend([
        item(1, "notes.txt", ItemKind::RegularFile, Some(20)),
        item(2, "photo.jpg", ItemKind::RegularFile, Some(200)),
        item(3, "projects", ItemKind::Directory, None),
    ]);
    let filter = DirectoryFilter::new("note").with_kind(ItemKind::RegularFile);
    let metadata_filter = DirectoryFilter::from_query(
        SearchQuery::parse("kind:file size:10..30 modified:1..2").unwrap(),
    )
    .unwrap();

    assert_eq!(filter.apply(&directory).len(), 1);
    assert_eq!(directory.items().len(), 3);
    assert_eq!(metadata_filter.apply(&directory).len(), 1);
    assert_eq!(DirectoryFilter::default().apply(&directory).len(), 3);
    for expression in ["hidden:true", "follow-links:true"] {
        assert!(
            DirectoryFilter::from_query(SearchQuery::parse(expression).unwrap()).is_err(),
            "directory filter silently accepted {expression}"
        );
    }
}

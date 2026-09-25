use musheen_core::CancellationToken;
use musheen_core::ItemKind;
use musheen_desktop::{PreviewDocument, PreviewLimits};
use musheen_ui::{
    InfoPaneDetails, InfoPaneModel, InfoPaneResult, InfoPaneState, PreviewPresentation,
};
use std::path::PathBuf;

fn details(name: &str) -> InfoPaneDetails {
    InfoPaneDetails::new(name, ItemKind::RegularFile, Some(42), Some(10))
}

#[test]
fn selection_states_are_explicit_and_new_work_cancels_old_work() {
    let mut model = InfoPaneModel::default();
    assert!(matches!(model.state(), InfoPaneState::Empty));

    let first = model.begin(details("first.txt"), PathBuf::from("/tmp/first.txt"));
    assert!(matches!(model.state(), InfoPaneState::Loading { .. }));
    let second = model.begin(details("second.txt"), PathBuf::from("/tmp/second.txt"));
    assert!(first.cancellation().is_cancelled());
    assert!(!second.cancellation().is_cancelled());

    assert!(!model.complete(
        first.generation(),
        InfoPaneResult::Details {
            mime_type: "text/plain".into(),
        },
    ));
    assert!(matches!(model.state(), InfoPaneState::Loading { .. }));
    assert!(model.complete(
        second.generation(),
        InfoPaneResult::Details {
            mime_type: "text/plain".into(),
        },
    ));
    assert!(matches!(
        model.state(),
        InfoPaneState::Ready {
            preview: PreviewPresentation::DetailsOnly,
            ..
        }
    ));

    model.show_multiple(3);
    assert!(second.cancellation().is_cancelled());
    assert!(matches!(
        model.state(),
        InfoPaneState::Multiple { count: 3 }
    ));
}

#[test]
fn matching_errors_are_visible_and_can_be_retried() {
    let mut model = InfoPaneModel::default();
    let work = model.begin(details("broken.bin"), PathBuf::from("/tmp/broken.bin"));

    assert!(model.fail(work.generation(), "Permission denied"));
    assert!(matches!(
        model.state(),
        InfoPaneState::Error { message, .. } if message.as_ref() == "Permission denied"
    ));

    let retry = model.retry().expect("failed local preview can be retried");
    assert!(matches!(model.state(), InfoPaneState::Loading { .. }));
    assert_ne!(work.generation(), retry.generation());
}

#[test]
fn load_more_moves_the_document_through_a_new_cancellable_generation() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("long.txt");
    std::fs::write(&path, b"abcdefghijklmnopqrstuvwxyz").unwrap();
    let document = PreviewDocument::open_with_limits(
        &path,
        CancellationToken::new(),
        PreviewLimits::new(4, 5, 14).unwrap(),
    )
    .unwrap();
    let mut model = InfoPaneModel::default();
    let work = model.begin(details("long.txt"), path);
    assert!(model.complete(
        work.generation(),
        InfoPaneResult::Preview {
            mime_type: "text/plain".into(),
            document,
        }
    ));

    let mut load = model
        .begin_load_more()
        .expect("text previews can load more");
    let cancellation = load.cancellation().clone();
    assert!(load.document_mut().load_more(cancellation).unwrap());
    let generation = load.generation();
    let (mime_type, document) = load.into_result_parts();
    assert!(model.complete(
        generation,
        InfoPaneResult::Preview {
            mime_type,
            document,
        }
    ));
    assert!(matches!(
        model.state(),
        InfoPaneState::Ready {
            preview: PreviewPresentation::Text(document),
            ..
        } if document.text() == Some("abcdefghi")
    ));
}

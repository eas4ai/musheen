use musheen_core::{CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState};

#[test]
fn every_capability_has_an_explicit_state() {
    let matrix = CapabilityMatrix::new(|kind| match kind {
        CapabilityKind::Permissions | CapabilityKind::AtomicRename => CapabilityState::Supported,
        CapabilityKind::Trash => CapabilityState::Unsupported(
            CapabilityReason::new("no trash service").expect("the reason is not empty"),
        ),
        _ => CapabilityState::Unknown(
            CapabilityReason::new("provider has not probed this capability")
                .expect("the reason is not empty"),
        ),
    });

    for kind in CapabilityKind::ALL {
        let state = matrix.get(kind);
        assert!(
            matches!(state, CapabilityState::Supported)
                || state.reason().is_some_and(|reason| !reason.is_empty()),
            "{kind:?} must be supported or carry a reason"
        );
    }
}

#[test]
fn capability_inventory_covers_the_portable_contract() {
    assert_eq!(CapabilityKind::ALL.len(), 11);
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::Permissions));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::Ownership));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::SymbolicLinks));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::HardLinks));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::SparseFiles));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::ExtendedAttributes));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::ReflinkCopies));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::Trash));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::AtomicRename));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::Watching));
    assert!(CapabilityKind::ALL.contains(&CapabilityKind::CaseSensitivity));
}

#[test]
fn capability_reasons_require_visible_text() {
    assert!(CapabilityReason::new("").is_err());
    assert!(CapabilityReason::new("   ").is_err());
}

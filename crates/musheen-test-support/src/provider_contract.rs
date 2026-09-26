use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityState, MutationRequest, PageRequest,
    ResourceLimits, Store, StoreError, StorePath,
};
use std::collections::HashMap;

/// Exercises behavior every read-only provider must share.
pub async fn verify_read_only_provider(
    store: &dyn Store,
    location: StorePath,
) -> Result<(), StoreError> {
    verify_capabilities(store, &location)?;
    let first = read_first_page(store, &location).await?;
    let repeated = read_first_page(store, &location).await?;
    verify_stable_ids(&first, &repeated)?;
    cancel_continuation(store, &location, first.next_request()).await?;
    cancel_continuation(store, &location, repeated.next_request()).await?;
    verify_mutation_refusal(store, location).await
}

fn verify_capabilities(store: &dyn Store, location: &StorePath) -> Result<(), StoreError> {
    let capabilities = store.capabilities(location);
    for kind in CapabilityKind::ALL {
        let state = capabilities.get(kind);
        if !matches!(state, CapabilityState::Supported) && state.reason().is_none_or(str::is_empty)
        {
            return Err(contract_failure(format!(
                "capability {kind:?} has no explicit state reason"
            )));
        }
    }
    Ok(())
}

async fn read_first_page(
    store: &dyn Store,
    location: &StorePath,
) -> Result<musheen_core::Page<musheen_core::StoreItem>, StoreError> {
    let page = store
        .read_directory(
            location,
            PageRequest::first(&ResourceLimits::default()),
            CancellationToken::new(),
        )
        .await?;
    if page.items().len() > ResourceLimits::default().directory_page_items() {
        return Err(contract_failure(
            "provider exceeded the requested page size",
        ));
    }
    Ok(page)
}

fn verify_stable_ids(
    first: &musheen_core::Page<musheen_core::StoreItem>,
    repeated: &musheen_core::Page<musheen_core::StoreItem>,
) -> Result<(), StoreError> {
    let first_ids = first
        .items()
        .iter()
        .map(|item| (item.path().clone(), item.id().clone()))
        .collect::<HashMap<_, _>>();
    for item in repeated.items() {
        if let Some(first_id) = first_ids.get(item.path())
            && first_id != item.id()
        {
            return Err(contract_failure(
                "provider changed a stable item ID between enumerations",
            ));
        }
    }
    Ok(())
}

async fn verify_mutation_refusal(store: &dyn Store, location: StorePath) -> Result<(), StoreError> {
    let mutation = MutationRequest::trash(location);
    if !matches!(
        store.validate_mutation(&mutation),
        Err(StoreError::Unsupported { .. })
    ) {
        return Err(contract_failure(
            "read-only provider accepted mutation validation",
        ));
    }
    if !matches!(
        store.mutate(mutation, CancellationToken::new()).await,
        Err(StoreError::Unsupported { .. })
    ) {
        return Err(contract_failure(
            "read-only provider started or accepted a mutation",
        ));
    }
    Ok(())
}

async fn cancel_continuation(
    store: &dyn Store,
    location: &StorePath,
    request: Option<PageRequest>,
) -> Result<(), StoreError> {
    let Some(request) = request else {
        return Ok(());
    };
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    match store.read_directory(location, request, cancellation).await {
        Err(StoreError::Cancelled) => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(contract_failure(
            "provider ignored cancellation between directory pages",
        )),
    }
}

fn contract_failure(message: impl Into<Box<str>>) -> StoreError {
    StoreError::Backend(message.into())
}

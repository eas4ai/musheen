use musheen_core::{CancellationToken, StoreError};
use std::future::poll_fn;
use std::task::Poll;

/// A virtual directory fixture that computes entries from their index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MillionItemFixture {
    total_items: usize,
    delay_yields: usize,
}

impl MillionItemFixture {
    pub const MAX_ITEMS: usize = 10_000_000;

    pub fn new(total_items: usize) -> Result<Self, StoreError> {
        if total_items > Self::MAX_ITEMS {
            return Err(StoreError::ResourceLimit {
                resource: "virtual fixture items",
                value: total_items,
                maximum: Self::MAX_ITEMS,
            });
        }
        Ok(Self {
            total_items,
            delay_yields: 1,
        })
    }

    #[must_use]
    pub fn with_delay_yields(mut self, delay_yields: usize) -> Self {
        self.delay_yields = delay_yields;
        self
    }

    #[must_use]
    pub fn total_items(self) -> usize {
        self.total_items
    }

    pub(crate) async fn delay(self, cancellation: &CancellationToken) -> Result<(), StoreError> {
        for _ in 0..self.delay_yields {
            let mut yielded = false;
            poll_fn(|context| {
                if yielded {
                    Poll::Ready(())
                } else {
                    yielded = true;
                    context.waker().wake_by_ref();
                    Poll::Pending
                }
            })
            .await;
            cancellation.check()?;
        }
        Ok(())
    }
}

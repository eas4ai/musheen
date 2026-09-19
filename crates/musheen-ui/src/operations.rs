pub use musheen_local::{
    DropAction, DropError, FileDragPayload, LocalOperationQueue, ReadyLocalOperation,
};

use gpui_kit::{AppContext, Context};
use std::sync::{Arc, Mutex};

pub(crate) fn spawn_ready_local_operations<V>(
    queue: Arc<Mutex<LocalOperationQueue>>,
    cx: &mut Context<V>,
    on_finish: impl Fn(&mut V, bool, Option<Box<str>>, &mut Context<V>) + Clone + 'static,
) -> Result<(), Box<str>>
where
    V: 'static,
{
    let ready = queue
        .lock()
        .map_err(|_| Box::<str>::from("the operation queue lock is poisoned"))?
        .start_ready()
        .map_err(|error| Box::<str>::from(error.to_string()))?;

    for operation in ready {
        let id = operation.id();
        let queue = Arc::clone(&queue);
        let on_finish = on_finish.clone();
        let work = cx.background_spawn(async move { operation.execute() });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let succeeded = result.is_ok();
            let failure = result.as_ref().err().cloned();
            let finish = queue
                .lock()
                .map_err(|_| "the operation queue lock is poisoned".to_owned())
                .and_then(|mut queue| queue.finish(id, result).map_err(|error| error.to_string()));
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                let error = finish.err().map(Into::into).or(failure);
                on_finish(state, succeeded, error, cx);
            });
        })
        .detach();
    }
    Ok(())
}

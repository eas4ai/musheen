use gpui_kit::{App, Global};

/// Owns native-theme's RAII subscription for exactly the application lifetime.
/// Dropping the GPUI global stops and joins the platform watcher.
struct NativeThemeWatcher {
    _subscription: native_theme::watch::ThemeSubscription,
}

impl Global for NativeThemeWatcher {}

pub(super) struct RefreshSignal(async_channel::Sender<()>);

impl RefreshSignal {
    pub(super) fn notify(&self) {
        // A pending signal is sufficient because the consumer re-reads the
        // current theme. Coalescing bounds memory during desktop signal bursts.
        let _ = self.0.try_send(());
    }
}

pub(super) fn refresh_signals() -> (RefreshSignal, async_channel::Receiver<()>) {
    let (sender, receiver) = async_channel::bounded(1);
    (RefreshSignal(sender), receiver)
}

/// Start the platform watcher once. Theme extraction runs off the GPUI thread;
/// installation and user-override replay always run on it.
pub(crate) fn install(cx: &mut App) {
    if cx.has_global::<NativeThemeWatcher>() {
        return;
    }
    let (signal, receiver) = refresh_signals();
    let subscription = match native_theme::watch::on_theme_change(move |_| signal.notify()) {
        Ok(subscription) => subscription,
        Err(error) => {
            eprintln!("Musheen could not watch the system theme: {error}");
            return;
        }
    };
    cx.set_global(NativeThemeWatcher {
        _subscription: subscription,
    });
    cx.spawn(async move |cx| {
        while receiver.recv().await.is_ok() {
            let load = cx
                .background_executor()
                .spawn(async { native_theme::SystemTheme::from_system() });
            match load.await {
                Ok(system) => cx.update(|cx| {
                    native_theme_gpui::apply_system_theme(&system, cx);
                    crate::settings::accept_native_theme_change(cx);
                }),
                Err(error) => {
                    eprintln!("Musheen could not refresh the system theme: {error}");
                }
            }
        }
    })
    .detach();
}

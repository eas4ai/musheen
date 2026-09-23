use std::path::PathBuf;

mod instance;

fn main() {
    let _instance = match instance::acquire_for_current_user() {
        Ok(instance::InstanceStatus::Primary(instance)) => instance,
        Ok(instance::InstanceStatus::AlreadyRunning) => {
            eprintln!("Musheen is already running; refusing a concurrent recovery process");
            return;
        }
        Err(error) => {
            eprintln!("Musheen could not acquire its instance lock: {error}");
            return;
        }
    };
    let initial_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    musheen_ui::run(initial_path);
}

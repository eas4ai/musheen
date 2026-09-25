use musheen_core::{DisplayPath, StorePath};

fn operate_on(_path: &StorePath) {}

fn main() {
    let display = DisplayPath::new("/tmp/example");
    operate_on(&display);
}

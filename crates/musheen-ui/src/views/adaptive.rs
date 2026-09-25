use super::Layout;

pub struct AdaptiveLayout;

impl AdaptiveLayout {
    #[must_use]
    pub fn resolve(width: f32) -> Layout {
        if width < 960.0 {
            Layout::List
        } else if width < 1_280.0 {
            Layout::Details
        } else {
            Layout::Grid
        }
    }
}

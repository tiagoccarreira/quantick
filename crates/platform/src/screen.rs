//! The Mac's main display: the screen that holds the menu bar.
//!
//! A window created with no position opens on the display that is active,
//! which is whichever screen the pointer is on. The main display is a
//! different fact. `NSScreen.mainScreen` is the screen of the key window, so
//! it is not the one this module asks for; the first screen AppKit lists is.

/// The usable rectangle of one display, in top-left logical points: the same
/// space a window position uses. The menu bar and the dock are outside it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkArea {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl WorkArea {
    fn new(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        let area = Self {
            x: x as f32,
            y: y as f32,
            width: width as f32,
            height: height as f32,
        };
        (area.x.is_finite()
            && area.y.is_finite()
            && area.width.is_finite()
            && area.height.is_finite()
            && area.width > 1.0
            && area.height > 1.0)
            .then_some(area)
    }
}

/// The usable rectangle of the display that holds the menu bar.
///
/// `None` off the main thread, when the machine has no display, and on every
/// target but macOS.
pub fn primary_work_area() -> Option<WorkArea> {
    #[cfg(target_os = "macos")]
    {
        macos_primary()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Where a window of `size` sits so it is centered in `area`.
///
/// A window larger than the area is pinned to the area's top-left, so its
/// title bar stays on that display. `None` when `size` is not a real rectangle.
pub fn center_in(area: WorkArea, size: [f32; 2]) -> Option<[f32; 2]> {
    let [width, height] = size;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    Some([
        area.x + ((area.width - width) * 0.5).max(0.0),
        area.y + ((area.height - height) * 0.5).max(0.0),
    ])
}

#[cfg(target_os = "macos")]
fn macos_primary() -> Option<WorkArea> {
    use core_graphics::display::CGDisplay;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::MainThreadMarker;

    let marker = MainThreadMarker::new()?;
    let screens = NSScreen::screens(marker);
    if screens.count() == 0 {
        return None;
    }
    // SAFETY: `count` is non-zero, so index 0 is a live `NSScreen`. AppKit
    // documents that slot as the display that holds the menu bar.
    let primary = unsafe { screens.objectAtIndex(0) };
    let visible = primary.visibleFrame();
    let main_height = CGDisplay::main().bounds().size.height;
    let y = main_height - visible.size.height - visible.origin.y;
    WorkArea::new(
        visible.origin.x,
        y,
        visible.size.width,
        visible.size.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> WorkArea {
        WorkArea {
            x: 0.0,
            y: 25.0,
            width: 1440.0,
            height: 800.0,
        }
    }

    #[test]
    fn a_window_centers_in_the_work_area() {
        assert_eq!(center_in(area(), [1100.0, 650.0]), Some([170.0, 100.0]));
    }

    #[test]
    fn a_window_larger_than_the_area_pins_to_its_top_left() {
        assert_eq!(center_in(area(), [2000.0, 1200.0]), Some([0.0, 25.0]));
    }

    #[test]
    fn a_non_finite_size_places_nowhere() {
        assert_eq!(center_in(area(), [f32::NAN, 100.0]), None);
        assert_eq!(center_in(area(), [100.0, 0.0]), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_primary_work_area_is_a_real_screen() {
        let Some(area) = primary_work_area() else {
            return;
        };
        assert!(area.width > 1.0 && area.height > 1.0, "{area:?}");
    }
}

//! Keep the editor's screen coordinates stable during a floating AppKit resize.

use cocoa::appkit::{NSScreen, NSView, NSWindow, NSWindowStyleMask};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSPoint, NSRect};
use objc::runtime::{BOOL, NO};
use objc::{msg_send, sel, sel_impl};
use std::time::{Duration, Instant};

const WINDOW_INSET: f64 = 1.0;
const MAX_WINDOW_CHROME_WIDTH: f64 = 64.0;
const MAX_WINDOW_CHROME_HEIGHT: f64 = 120.0;
const MAX_PENDING_TIME: Duration = Duration::from_millis(250);

#[derive(Clone, Copy)]
pub(super) struct WindowAnchor {
    window: id,
    width: u32,
    height: u32,
    outer_width: f64,
    editor_origin: NSPoint,
    recorded: Instant,
}

impl WindowAnchor {
    /// Take a screen-space anchor before calling the host's resize callback.
    ///
    /// The parent must be a live host NSView on the AppKit thread.
    pub(super) unsafe fn capture(
        parent: id,
        previous_size: (i32, i32),
        width: u32,
        height: u32,
        left_delta: i32,
    ) -> Option<Self> {
        if parent.is_null() {
            return None;
        }
        let (previous_width, previous_height) = previous_size;
        let window: id = unsafe { msg_send![parent, window] };
        if window.is_null() {
            return None;
        }
        let (frame, screen) = unsafe {
            let movable: BOOL = msg_send![window, isMovable];
            let sheet: BOOL = msg_send![window, isSheet];
            // A normal-level project window or an embedded editor belongs to
            // the host's layout. Only a dedicated floating window can move.
            if movable == NO
                || sheet != NO
                || window.level() <= 0
                || window
                    .styleMask()
                    .contains(NSWindowStyleMask::NSFullScreenWindowMask)
            {
                return None;
            }
            (NSWindow::frame(window), window.screen())
        };
        if screen.is_null()
            || !(-WINDOW_INSET..=MAX_WINDOW_CHROME_WIDTH)
                .contains(&(frame.size.width - f64::from(previous_width)))
            || !(-WINDOW_INSET..=MAX_WINDOW_CHROME_HEIGHT)
                .contains(&(frame.size.height - f64::from(previous_height)))
        {
            return None;
        }

        let visible = unsafe { screen.visibleFrame() };
        let outer_width = frame.size.width + f64::from(width) - f64::from(previous_width);
        let expected_left = frame.origin.x - f64::from(left_delta);
        if left_delta > 0
            && (expected_left < visible.origin.x - WINDOW_INSET
                || expected_left + outer_width
                    > visible.origin.x + visible.size.width + WINDOW_INSET)
        {
            return None;
        }

        let mut editor_origin = unsafe { screen_origin(parent, window) };
        editor_origin.x -= f64::from(left_delta);
        Some(Self {
            window,
            width,
            height,
            outer_width,
            editor_origin,
            recorded: Instant::now(),
        })
    }

    pub(super) fn expired(self) -> bool {
        self.recorded.elapsed() > MAX_PENDING_TIME
    }

    pub(super) fn matches(self, width: i32, height: i32) -> bool {
        (width, height) == (self.width as i32, self.height as i32)
    }

    /// Move only the position of the same floating window after host resize.
    ///
    /// The supplied parent must still be the live attached NSView.
    pub(super) unsafe fn restore(self, parent: id) {
        if parent.is_null() {
            return;
        }
        let window: id = unsafe { msg_send![parent, window] };
        if window != self.window {
            return;
        }
        let frame = unsafe { NSWindow::frame(window) };
        let screen = unsafe { window.screen() };
        // The window itself must have accommodated the requested view size.
        // In particular, never move a large window with a resized child.
        if screen.is_null() || (frame.size.width - self.outer_width).abs() > WINDOW_INSET {
            return;
        }
        let visible = unsafe { screen.visibleFrame() };
        let max_x = visible.origin.x + visible.size.width - frame.size.width;
        if max_x < visible.origin.x {
            return;
        }
        let current = unsafe { screen_origin(parent, window) };
        let x = (frame.origin.x + self.editor_origin.x - current.x).clamp(visible.origin.x, max_x);
        let max_y = visible.origin.y + visible.size.height - frame.size.height;
        let y = if max_y >= visible.origin.y {
            (frame.origin.y + self.editor_origin.y - current.y).clamp(visible.origin.y, max_y)
        } else {
            frame.origin.y
        };
        if (x - frame.origin.x).abs() > 0.01 || (y - frame.origin.y).abs() > 0.01 {
            unsafe { window.setFrameOrigin_(NSPoint::new(x, y)) };
        }
    }
}

unsafe fn screen_origin(parent: id, window: id) -> NSPoint {
    let bounds = unsafe { parent.bounds() };
    let window_rect: NSRect = unsafe { msg_send![parent, convertRect: bounds toView: nil] };
    unsafe { window.convertRectToScreen_(window_rect) }.origin
}

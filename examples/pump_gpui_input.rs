//! Exercise Pump's native macOS GPUI input path in an isolated AppKit host.

#[cfg(target_os = "macos")]
mod macos {
    use cocoa::appkit::{NSApp, NSBackingStoreType, NSView, NSWindow, NSWindowStyleMask};
    use cocoa::base::{id, nil};
    use cocoa::foundation::{NSAutoreleasePool, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};
    use objc::runtime::{Object, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use pmpx::gui_gpui::{new_screenshot_gui_with_params, WINDOW_HEIGHT, WINDOW_WIDTH};
    use raw_window_handle::{AppKitWindowHandle, RawWindowHandle};
    use std::ffi::{c_void, CStr, CString, OsString};
    use std::path::PathBuf;
    use std::ptr::NonNull;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const OUTPUT_WIDTH: u32 = WINDOW_WIDTH;
    const OUTPUT_HEIGHT: u32 = WINDOW_HEIGHT;
    const PANEL_WIDTH: u32 = 240;
    const COMMAND: u64 = 1_u64 << 20;
    const SHIFT: u64 = 1_u64 << 17;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateScrollWheelEvent2(
            source: *mut c_void,
            units: u32,
            wheel_count: u32,
            wheel1: i32,
            wheel2: i32,
            wheel3: i32,
        ) -> *mut c_void;
        fn CGEventSetLocation(event: *mut c_void, location: NSPoint);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const c_void);
    }

    struct NativeFixture {
        window: id,
        view: id,
    }

    impl NativeFixture {
        unsafe fn new() -> Self {
            let _ = NSApp();
            let frame = NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(f64::from(OUTPUT_WIDTH), f64::from(OUTPUT_HEIGHT)),
            );
            let window = NSWindow::alloc(nil).initWithContentRect_styleMask_backing_defer_(
                frame,
                NSWindowStyleMask::NSBorderlessWindowMask,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            );
            let view = NSView::alloc(nil).initWithFrame_(frame);
            window.setContentView_(view);
            window.makeKeyAndOrderFront_(nil);
            Self { window, view }
        }

        fn parent_handle(&self) -> RawWindowHandle {
            let mut handle = AppKitWindowHandle::empty();
            handle.ns_view = NonNull::new(self.view as *mut _)
                .expect("fixture view pointer")
                .as_ptr();
            RawWindowHandle::AppKit(handle)
        }

        unsafe fn sync_host_size(&self, width: u32) {
            let bounds: NSRect = msg_send![self.hosted_view(), bounds];
            assert_eq!(
                (bounds.size.width, bounds.size.height),
                (f64::from(width), f64::from(OUTPUT_HEIGHT))
            );
            self.window
                .setContentSize_(NSSize::new(f64::from(width), f64::from(OUTPUT_HEIGHT)));
        }

        unsafe fn hosted_view(&self) -> id {
            let subviews: id = msg_send![self.view, subviews];
            let count: usize = msg_send![subviews, count];
            assert!(count > 0, "Pump GPUI should attach a native child view");
            msg_send![subviews, objectAtIndex: count - 1]
        }
    }

    impl Drop for NativeFixture {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.window, setContentView: nil];
                let _: () = msg_send![self.view, release];
                let _: () = msg_send![self.window, release];
            }
        }
    }

    struct CurveSlotSandbox {
        directory: PathBuf,
        previous: Option<OsString>,
    }

    impl CurveSlotSandbox {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after the Unix epoch")
                .as_nanos();
            let directory = std::env::temp_dir()
                .join(format!("pump-gpui-input-{}-{stamp}", std::process::id()));
            std::fs::create_dir_all(&directory).expect("curve slot sandbox directory");
            let previous = std::env::var_os("PUMP_GLOBAL_CURVE_SLOTS_PATH");
            std::env::set_var(
                "PUMP_GLOBAL_CURVE_SLOTS_PATH",
                directory.join("curve-slots.bin"),
            );
            Self {
                directory,
                previous,
            }
        }
    }

    impl Drop for CurveSlotSandbox {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("PUMP_GLOBAL_CURVE_SLOTS_PATH", value),
                None => std::env::remove_var("PUMP_GLOBAL_CURVE_SLOTS_PATH"),
            }
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    struct PasteboardSnapshot {
        items: Vec<PasteboardItemSnapshot>,
    }

    struct PasteboardItemSnapshot {
        types: Vec<(String, Vec<u8>)>,
    }

    struct PasteboardRestore(PasteboardSnapshot);

    impl PasteboardRestore {
        unsafe fn capture() -> Self {
            Self(PasteboardSnapshot::capture())
        }
    }

    impl Drop for PasteboardRestore {
        fn drop(&mut self) {
            unsafe {
                self.0.restore();
            }
        }
    }

    impl PasteboardSnapshot {
        unsafe fn capture() -> Self {
            let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
            if pasteboard.is_null() {
                return Self { items: Vec::new() };
            }
            let pasteboard_items: id = msg_send![pasteboard, pasteboardItems];
            if pasteboard_items.is_null() {
                return Self { items: Vec::new() };
            }
            let item_count: usize = msg_send![pasteboard_items, count];
            let mut items = Vec::with_capacity(item_count);
            for item_index in 0..item_count {
                let item: id = msg_send![pasteboard_items, objectAtIndex: item_index];
                if item.is_null() {
                    continue;
                }
                let item_types: id = msg_send![item, types];
                if item_types.is_null() {
                    items.push(PasteboardItemSnapshot { types: Vec::new() });
                    continue;
                }
                let type_count: usize = msg_send![item_types, count];
                let mut types = Vec::with_capacity(type_count);
                for type_index in 0..type_count {
                    let type_object: id = msg_send![item_types, objectAtIndex: type_index];
                    let type_name =
                        ns_string(type_object).expect("pasteboard type should expose UTF-8 text");
                    let data: id = msg_send![item, dataForType: type_object];
                    let data_length = if data.is_null() {
                        0
                    } else {
                        msg_send![data, length]
                    };
                    let bytes = if data_length == 0 {
                        Vec::new()
                    } else {
                        let pointer: *const u8 = msg_send![data, bytes];
                        assert!(!pointer.is_null(), "pasteboard data should have bytes");
                        std::slice::from_raw_parts(pointer, data_length).to_vec()
                    };
                    types.push((type_name, bytes));
                }
                items.push(PasteboardItemSnapshot { types });
            }
            Self { items }
        }

        unsafe fn restore(&self) {
            let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
            if pasteboard.is_null() {
                return;
            }
            let _: isize = msg_send![pasteboard, clearContents];
            if self.items.is_empty() {
                return;
            }
            let objects: id =
                msg_send![class!(NSMutableArray), arrayWithCapacity: self.items.len()];
            if objects.is_null() {
                return;
            }
            for snapshot in &self.items {
                let allocated: id = msg_send![class!(NSPasteboardItem), alloc];
                let item: id = msg_send![allocated, init];
                if item.is_null() {
                    continue;
                }
                for (type_name, bytes) in &snapshot.types {
                    let type_c_string = CString::new(type_name.as_bytes())
                        .expect("pasteboard type should not contain nul");
                    let type_object: id = msg_send![
                        class!(NSString),
                        stringWithUTF8String: type_c_string.as_ptr()
                    ];
                    if type_object.is_null() {
                        continue;
                    }
                    let data: id = msg_send![
                        class!(NSData),
                        dataWithBytes: bytes.as_ptr()
                        length: bytes.len()
                    ];
                    let _: BOOL = msg_send![item, setData: data forType: type_object];
                }
                let _: () = msg_send![objects, addObject: item];
                let _: () = msg_send![item, release];
            }
            let _: BOOL = msg_send![pasteboard, writeObjects: objects];
        }
    }

    unsafe fn ns_string(value: id) -> Option<String> {
        if value.is_null() {
            return None;
        }
        let pointer: *const i8 = msg_send![value, UTF8String];
        if pointer.is_null() {
            return None;
        }
        CStr::from_ptr(pointer).to_str().ok().map(str::to_owned)
    }

    unsafe fn pasteboard_string() -> Option<String> {
        let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
        let type_name = CString::new("public.utf8-plain-text").expect("static type name");
        let type_object: id = msg_send![
            class!(NSString),
            stringWithUTF8String: type_name.as_ptr()
        ];
        let value: id = msg_send![pasteboard, stringForType: type_object];
        ns_string(value)
    }

    unsafe fn set_pasteboard_string(value: &str) {
        let pasteboard: id = msg_send![class!(NSPasteboard), generalPasteboard];
        let type_name = CString::new("public.utf8-plain-text").expect("static type name");
        let type_object: id = msg_send![
            class!(NSString),
            stringWithUTF8String: type_name.as_ptr()
        ];
        let value = CString::new(value).expect("pasteboard text has no nul");
        let value_object: id = msg_send![
            class!(NSString),
            stringWithUTF8String: value.as_ptr()
        ];
        let _: isize = msg_send![pasteboard, clearContents];
        let _: BOOL = msg_send![pasteboard, setString: value_object forType: type_object];
    }

    unsafe fn pump_appkit(app: id, gui: &toybox::gpui_gui::GpuiHostedGui, seconds: f64) {
        pump_appkit_events(app, seconds, || gui.pump());
    }

    unsafe fn pump_appkit_events(app: id, seconds: f64, mut after_events: impl FnMut()) {
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < deadline {
            let date: id = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: 0.004_f64];
            let event: id = msg_send![
                app,
                nextEventMatchingMask: usize::MAX
                untilDate: date
                inMode: NSDefaultRunLoopMode
                dequeue: YES
            ];
            if !event.is_null() {
                let _: () = msg_send![app, sendEvent: event];
            }
            let _: () = msg_send![app, updateWindows];
            after_events();
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn capture_frame(gui: &toybox::gpui_gui::GpuiHostedGui, context: &str) {
        let (width, height, pixels) = capture_pixels(gui, context);
        assert!(
            width > 0 && height > 0 && !pixels.is_empty(),
            "{context} should produce visible GPUI pixels"
        );
    }

    fn capture_pixels(gui: &toybox::gpui_gui::GpuiHostedGui, context: &str) -> (u32, u32, Vec<u8>) {
        let capture = gui
            .capture_rgba()
            .unwrap_or_else(|error| panic!("{context} should render a GPUI frame: {error}"));
        capture
    }

    fn count_plot_pixels(
        gui: &toybox::gpui_gui::GpuiHostedGui,
        color_matches: impl Fn(&[u8]) -> bool,
    ) -> usize {
        count_region_pixels(gui, "curve-plot", color_matches)
    }

    fn count_region_pixels(
        gui: &toybox::gpui_gui::GpuiHostedGui,
        region: &str,
        color_matches: impl Fn(&[u8]) -> bool,
    ) -> usize {
        let (width, height, pixels) = capture_pixels(gui, region);
        let [left, top, plot_width, plot_height] = pmpx::gui_gpui::screenshot_bounds(region);
        let scale = height as f32 / OUTPUT_HEIGHT as f32;
        let left_px = (left * scale).ceil() as usize;
        let right_px = ((left + plot_width) * scale).floor() as usize;
        let top_px = (top * scale).ceil() as usize;
        let bottom_px = ((top + plot_height) * scale).floor() as usize;
        pixels
            .chunks_exact(width as usize * 4)
            .take(bottom_px)
            .skip(top_px)
            .flat_map(|row| row[left_px * 4..right_px * 4].as_chunks::<4>().0.iter())
            .filter(|&pixel| color_matches(pixel))
            .count()
    }

    unsafe fn send_mouse_event(window: id, event_type: usize, x: f64, top_y: f64, modifiers: u64) {
        let window_number: isize = msg_send![window, windowNumber];
        let location = NSPoint::new(x, f64::from(OUTPUT_HEIGHT) - top_y);
        let event: id = msg_send![
            class!(NSEvent),
            mouseEventWithType: event_type
            location: location
            modifierFlags: modifiers
            timestamp: 0.0_f64
            windowNumber: window_number
            context: std::ptr::null_mut::<Object>()
            eventNumber: 1_isize
            clickCount: 1_isize
            pressure: 1.0_f64
        ];
        if event_type == 5 {
            // WindowServer tracking-area events target their registered owner.
            // A raw NSWindow sendEvent(mouseMoved) instead targets the first
            // responder, so explicitly emulate the documented tracking route.
            let content: id = msg_send![window, contentView];
            let children: id = msg_send![content, subviews];
            let child: id = msg_send![children, lastObject];
            let areas: id = msg_send![child, trackingAreas];
            let area: id = msg_send![areas, firstObject];
            assert!(
                !area.is_null(),
                "native GPUI child must install a tracking area"
            );
            let owner: id = msg_send![area, owner];
            assert_eq!(owner, child, "tracking-area owner must be the GPUI child");
            let child_bounds: NSRect = msg_send![child, bounds];
            if x < 0.0
                || x >= child_bounds.size.width
                || top_y < 0.0
                || top_y >= f64::from(WINDOW_HEIGHT)
            {
                let _: () = msg_send![owner, mouseExited: event];
            } else {
                let _: () = msg_send![owner, mouseMoved: event];
            }
        } else {
            let _: () = msg_send![window, sendEvent: event];
        }
    }

    unsafe fn send_mouse_move(window: id, x: f64, top_y: f64) {
        send_mouse_move_with_modifiers(window, x, top_y, 0);
    }

    unsafe fn send_mouse_move_with_modifiers(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 5, x, top_y, modifiers);
    }

    unsafe fn send_mouse_down(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 1, x, top_y, modifiers);
    }

    unsafe fn send_mouse_dragged(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 6, x, top_y, modifiers);
    }

    unsafe fn send_mouse_up(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 2, x, top_y, modifiers);
    }

    unsafe fn send_secondary_mouse_down(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 3, x, top_y, modifiers);
    }

    unsafe fn send_secondary_mouse_dragged(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 7, x, top_y, modifiers);
    }

    unsafe fn send_secondary_mouse_up(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_event(window, 4, x, top_y, modifiers);
    }

    unsafe fn send_scroll_wheel(
        target_view: id,
        window: id,
        x: f64,
        top_y: f64,
        delta_y: f64,
        modifiers: u64,
    ) {
        let _ = modifiers;
        // AppKit has no public scroll-event constructor. Build a real
        // CoreGraphics event, wrap it as NSEvent, and deliver it to the
        // hosted NSView's native scrollWheel: entry point. Sending this local
        // event through NSApplication would require a global event post
        // because eventWithCGEvent: has no window number.
        let cg_event = CGEventCreateScrollWheelEvent2(
            std::ptr::null_mut(),
            1,
            1,
            delta_y.round() as i32,
            0,
            0,
        );
        assert!(
            !cg_event.is_null(),
            "CoreGraphics should create a scroll event"
        );
        let window_frame: NSRect = msg_send![window, frame];
        let screen: id = msg_send![window, screen];
        let screen_frame: NSRect = msg_send![screen, frame];
        let window_y = f64::from(OUTPUT_HEIGHT) - top_y;
        let appkit_global = NSPoint::new(
            screen_frame.origin.x + window_frame.origin.x + x,
            screen_frame.origin.y + window_frame.origin.y + window_y,
        );
        let quartz_location = NSPoint::new(
            appkit_global.x,
            screen_frame.origin.y + screen_frame.size.height - appkit_global.y,
        );
        CGEventSetLocation(cg_event, quartz_location);
        let event: id = msg_send![class!(NSEvent), eventWithCGEvent: cg_event];
        assert!(
            !event.is_null(),
            "AppKit should wrap the CoreGraphics scroll event"
        );
        let _: () = msg_send![target_view, scrollWheel: event];
        CFRelease(cg_event.cast_const());
    }

    unsafe fn send_click(window: id, x: f64, top_y: f64, modifiers: u64) {
        send_mouse_down(window, x, top_y, modifiers);
        send_mouse_up(window, x, top_y, modifiers);
    }

    unsafe fn send_key(
        app: id,
        window: id,
        gui: &toybox::gpui_gui::GpuiHostedGui,
        text: &str,
        key_code: u16,
        modifiers: u64,
    ) {
        send_key_with_repeat(app, window, gui, text, key_code, modifiers, false);
    }

    unsafe fn send_repeated_key(
        app: id,
        window: id,
        gui: &toybox::gpui_gui::GpuiHostedGui,
        text: &str,
        key_code: u16,
        modifiers: u64,
    ) {
        send_key_with_repeat(app, window, gui, text, key_code, modifiers, true);
    }

    unsafe fn send_key_with_repeat(
        app: id,
        window: id,
        gui: &toybox::gpui_gui::GpuiHostedGui,
        text: &str,
        key_code: u16,
        modifiers: u64,
        repeat: bool,
    ) {
        let bytes = CString::new(text).expect("key text has no nul");
        let characters: id = msg_send![
            class!(NSString),
            stringWithUTF8String: bytes.as_ptr()
        ];
        let window_number: isize = msg_send![window, windowNumber];
        let down: id = msg_send![
            class!(NSEvent),
            keyEventWithType: 10_usize
            location: NSPoint::new(48.0, 28.0)
            modifierFlags: modifiers
            timestamp: 0.0_f64
            windowNumber: window_number
            context: std::ptr::null_mut::<Object>()
            characters: characters
            charactersIgnoringModifiers: characters
            isARepeat: NO
            keyCode: key_code
        ];
        let _: () = msg_send![window, sendEvent: down];
        if repeat {
            let repeated_down: id = msg_send![
                class!(NSEvent),
                keyEventWithType: 10_usize
                location: NSPoint::new(48.0, 28.0)
                modifierFlags: modifiers
                timestamp: 0.0_f64
                windowNumber: window_number
                context: std::ptr::null_mut::<Object>()
                characters: characters
                charactersIgnoringModifiers: characters
                isARepeat: YES
                keyCode: key_code
            ];
            let _: () = msg_send![window, sendEvent: repeated_down];
        }
        let up: id = msg_send![
            class!(NSEvent),
            keyEventWithType: 11_usize
            location: NSPoint::new(48.0, 28.0)
            modifierFlags: modifiers
            timestamp: 0.0_f64
            windowNumber: window_number
            context: std::ptr::null_mut::<Object>()
            characters: characters
            charactersIgnoringModifiers: characters
            isARepeat: NO
            keyCode: key_code
        ];
        let _: () = msg_send![window, sendEvent: up];
        pump_appkit(app, gui, 0.02);
    }

    fn mac_key_code(character: char) -> u16 {
        match character {
            '0' => 29,
            '1' => 18,
            '2' => 19,
            '3' => 20,
            '4' => 21,
            '5' => 23,
            '6' => 22,
            '7' => 26,
            '8' => 28,
            '9' => 25,
            '.' => 47,
            '-' => 27,
            'x' => 7,
            ' ' => 49,
            'm' => 46,
            's' => 1,
            'z' => 6,
            _ => panic!("native input fixture has no key code for {character:?}"),
        }
    }

    unsafe fn send_text(app: id, window: id, gui: &toybox::gpui_gui::GpuiHostedGui, text: &str) {
        for character in text.chars() {
            let character_text = character.to_string();
            send_key(
                app,
                window,
                gui,
                &character_text,
                mac_key_code(character),
                0,
            );
        }
    }

    fn center(key: &str) -> (f64, f64) {
        let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds(key);
        (f64::from(x + w * 0.5), f64::from(y + h * 0.5))
    }

    unsafe fn wait_for_control(app: id, gui: &toybox::gpui_gui::GpuiHostedGui, key: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while pmpx::gui_gpui::screenshot_bounds_if_painted(key).is_none() {
            assert!(Instant::now() < deadline, "control {key} must paint");
            pump_appkit(app, gui, 0.01);
        }
    }

    unsafe fn click_control(fixture: &NativeFixture, key: &str, modifiers: u64) {
        let (x, y) = center(key);
        send_click(fixture.window, x, y, modifiers);
    }

    unsafe fn type_value(
        app: id,
        fixture: &NativeFixture,
        gui: &toybox::gpui_gui::GpuiHostedGui,
        key: &str,
        value: &str,
    ) {
        click_control(fixture, key, COMMAND);
        pump_appkit(app, gui, 0.03);
        send_key(app, fixture.window, gui, "a", 0, COMMAND);
        send_text(app, fixture.window, gui, value);
        send_key(app, fixture.window, gui, "\r", 36, 0);
        pump_appkit(app, gui, 0.03);
    }

    unsafe fn assert_delay_text_edit(
        app: id,
        fixture: &NativeFixture,
        gui: &toybox::gpui_gui::GpuiHostedGui,
        value: &str,
    ) {
        click_control(fixture, "delay-value", 0);
        pump_appkit(app, gui, 0.03);
        send_key(app, fixture.window, gui, "a", 0, COMMAND);
        send_text(app, fixture.window, gui, value);
        send_key(app, fixture.window, gui, "\r", 36, 0);
    }

    unsafe fn exercise_native_input(fixture: &NativeFixture) {
        let app = NSApp();
        let (mut gui, params, status) = new_screenshot_gui_with_params();
        gui.set_parent_raw(fixture.parent_handle());
        assert!(gui.open(), "Pump GPUI input fixture should open");
        gui.request_resize(OUTPUT_WIDTH, OUTPUT_HEIGHT);
        let _: () = msg_send![fixture.window, makeFirstResponder: std::ptr::null_mut::<Object>()];
        pump_appkit(app, &gui, 0.1);
        capture_frame(&gui, "opened instrument editor");
        let [_, plot_top, _, _] = pmpx::gui_gpui::screenshot_bounds("curve-plot");
        assert!(
            plot_top < 55.0,
            "the plot should start directly below the header"
        );

        // A flat mid-gain curve puts the two strokes at exactly the same height.
        let original_curve = params.editable_curve_snapshot();
        let mut flat_curve = original_curve.clone();
        for node in &mut flat_curve.nodes {
            node.y = 0.5;
        }
        params.set_editable_curve(&flat_curve);
        params.set_smooth(0.5);
        pump_appkit(app, &gui, 0.04);
        let pale_blue = |pixel: &[u8]| {
            pixel[0] > 140
                && pixel[1] > 195
                && pixel[2] > 232
                && pixel[2].saturating_sub(pixel[0]) > 60
        };
        assert!(
            count_plot_pixels(&gui, pale_blue) > 350,
            "the smoothed stroke should be visible over the editable stroke"
        );
        params.set_smooth(0.0);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(count_plot_pixels(&gui, pale_blue), 0);
        params.set_editable_curve(&original_curve);

        let transport = pmpx::GuiTransportTelemetry {
            is_playing: false,
            transport_is_playing: true,
            has_host_beats_timeline: false,
            beat_phase: 0.0,
            tempo_bpm: 120.0,
            beats_per_cycle: 1.0,
            delay_beats: 0,
            delay_progress: 0.0,
            timing_mode: 0,
            effective_cycle_rate_hz: 0.0,
        };
        status.update_transport(0.0, transport);
        status.publish_gain_reduction(0.25, true);
        pump_appkit(app, &gui, 0.06);
        let (width, height, pixels) = capture_pixels(&gui, "gain reduction meter");
        let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds("gain-reduction-meter");
        let meter_x = ((x + w * 0.5) * width as f32 / OUTPUT_WIDTH as f32) as usize;
        let meter_pixel_at = |fraction| {
            let row = ((y + h * fraction) * height as f32 / OUTPUT_HEIGHT as f32) as usize;
            let offset = (row * width as usize + meter_x) * 4;
            &pixels[offset..offset + 4]
        };
        assert!(
            meter_pixel_at(0.08)[0].saturating_sub(meter_pixel_at(0.92)[0]) > 60,
            "gain reduction should fill from the top of the meter"
        );
        let idle_track = meter_pixel_at(0.92);
        assert_eq!(idle_track[0], idle_track[1]);
        assert_eq!(idle_track[1], idle_track[2]);
        status.mark_gain_reduction_inactive();
        status.update_transport(
            0.0,
            pmpx::GuiTransportTelemetry {
                transport_is_playing: false,
                ..transport
            },
        );

        for key in [
            "timing-mode",
            "timing-value",
            "delay-value",
            "slider-Smooth",
            "slider-Swing",
            "knob-Mix",
            "knob-OutputGain",
            "value-Smooth",
            "value-Swing",
        ] {
            let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds(key);
            assert!(
                x >= 0.
                    && y >= 0.
                    && w > 0.
                    && h > 0.
                    && x + w <= OUTPUT_WIDTH as f32
                    && y + h <= OUTPUT_HEIGHT as f32,
                "{key} must remain inside the minimum editor: {x},{y},{w},{h}"
            );
        }

        // Timing, its menu, and the retained delay entry remain native controls.
        click_control(fixture, "timing-value", 0);
        pump_appkit(app, &gui, 0.04);
        click_control(fixture, "timing-sync-6", 0);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.sync_division(), 6);
        assert_delay_text_edit(app, fixture, &gui, "4");
        assert_eq!(params.delay_beats(), 4);
        for (character, code) in [(0, 1), (127, 0), (8, 0)] {
            click_control(fixture, "delay-value", 0);
            send_key(app, fixture.window, &gui, "a", 0, COMMAND);
            send_text(app, fixture.window, &gui, "12");
            assert!(
                gui.on_key_down(character, code, 0),
                "focused Backspace must be consumed"
            );
            gui.on_key_up(character, code, 0);
            send_key(app, fixture.window, &gui, "\r", 36, 0);
            assert_eq!(params.delay_beats(), 1);
        }
        assert_delay_text_edit(app, fixture, &gui, "4");
        let pasteboard_restore = PasteboardRestore::capture();
        click_control(fixture, "delay-value", 0);
        send_key(app, fixture.window, &gui, "a", 0, COMMAND);
        send_key(app, fixture.window, &gui, "c", 8, COMMAND);
        assert_eq!(pasteboard_string(), Some("4".to_owned()));
        set_pasteboard_string("6");
        send_key(app, fixture.window, &gui, "v", 9, COMMAND);
        send_key(app, fixture.window, &gui, "\r", 36, 0);
        assert_eq!(params.delay_beats(), 6);
        drop(pasteboard_restore);

        // Smooth and Swing now admit horizontal slider drags, including outside
        // the window, instead of the old vertical knob gesture.
        for key in ["Smooth", "Swing"] {
            let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds(&format!("slider-{key}"));
            let y = f64::from(y + h * 0.5);
            let start = f64::from(x + 5. + (w - 10.) * 0.25);
            let end = f64::from(x + 5. + (w - 10.) * 0.75);
            send_mouse_move(fixture.window, start, y);
            send_mouse_down(fixture.window, start, y, 0);
            send_mouse_dragged(fixture.window, end, y, 0);
            send_mouse_up(fixture.window, end, y, 0);
            pump_appkit(app, &gui, 0.04);
            let value = if key == "Smooth" {
                params.smooth()
            } else {
                params.swing()
            };
            assert!(
                (value - 0.75).abs() < 0.02,
                "{key} slider must follow its horizontal track: {value}"
            );
            type_value(app, fixture, &gui, &format!("value-{key}"), "25");
            let value = if key == "Smooth" {
                params.smooth()
            } else {
                params.swing()
            };
            assert!(
                (value - 0.25).abs() < 0.001,
                "{key} must retain native numeric entry"
            );
        }
        let [x, y, _, h] = pmpx::gui_gpui::screenshot_bounds("slider-Smooth");
        let y = f64::from(y + h * 0.5);
        send_mouse_down(fixture.window, f64::from(x + 10.), y, 0);
        send_mouse_dragged(fixture.window, 650., 180., 0);
        assert_eq!(params.smooth(), 1.);
        send_mouse_dragged(fixture.window, -20., 200., 0);
        assert_eq!(params.smooth(), 0.);
        send_mouse_up(fixture.window, -20., 200., 0);
        let (x, y) = center("slider-Smooth");
        send_scroll_wheel(fixture.hosted_view(), fixture.window, x, y, 12., 0);
        pump_appkit(app, &gui, 0.04);
        assert!(
            params.smooth() > 0.,
            "slider wheel must affect its parameter"
        );
        send_key(app, fixture.window, &gui, "\u{f700}", 126, 0);

        assert!(center("compressor-panel").0 < center("waveform-mode").0);
        assert!(center("filter-panel").0 > center("bypass").0);
        assert!(pmpx::gui_gpui::screenshot_bounds_if_painted("compressor-capture").is_none());
        let base_plot = pmpx::gui_gpui::screenshot_bounds("curve-plot");
        assert_eq!(params.effects()[0], 0.0);
        click_control(fixture, "filter-panel", 0);
        pump_appkit(app, &gui, 0.06);
        assert_eq!(params.effects()[0], 1.0);
        assert_eq!(params.compressor()[0], 0.0);
        fixture.sync_host_size(OUTPUT_WIDTH + PANEL_WIDTH);
        assert_eq!(pmpx::gui_gpui::screenshot_bounds("curve-plot"), base_plot);
        let [frequency_x, _, _, _] = pmpx::gui_gpui::screenshot_bounds("frequency-plot");
        assert!(frequency_x >= OUTPUT_WIDTH as f32);

        let spectrum_ink = |pixel: &[u8]| {
            // The crossover fills lightly tint the gray spectrum behind them.
            let [red, green, blue] = [pixel[0], pixel[1], pixel[2]];
            (68..150).contains(&red) && red.abs_diff(green) <= 12 && red.abs_diff(blue) <= 12
        };
        let before = count_region_pixels(&gui, "frequency-plot", spectrum_ink);
        pmpx::gui_gpui::seed_screenshot_track_spectrum(&status);
        pump_appkit(app, &gui, 0.04);
        let after = count_region_pixels(&gui, "frequency-plot", spectrum_ink);
        assert!(
            after > before + 75,
            "the spectrum should add visible ink ({before} to {after})"
        );

        let post_ink = |pixel: &[u8]| {
            pixel[0] > 100
                && pixel[0].saturating_sub(pixel[1]) > 28
                && pixel[0].saturating_sub(pixel[2]) > 35
        };
        assert!(count_region_pixels(&gui, "frequency-plot", post_ink) > 45);
        // Both band bars activate DSP without a separate switch, and one Undo
        // restores the entire drag including its activation.
        for index in [3, 4] {
            params.set_effect(0, 0.);
            params.set_effect(index, 1.);
            pump_appkit(app, &gui, 0.04);
            let curve = params.editable_curve_snapshot();
            let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds(&format!("band-{index}"));
            let y = f64::from(y + h * 0.5);
            let start = f64::from(x + w - 5.);
            let end = f64::from(x + 5. + (w - 10.) * 0.25);
            send_mouse_down(fixture.window, start, y, 0);
            send_mouse_dragged(fixture.window, end, y, 0);
            send_mouse_up(fixture.window, end, y, 0);
            pump_appkit(app, &gui, 0.04);
            assert_eq!(params.effects()[0], 1.);
            assert!((params.effects()[index] - 0.25).abs() < 0.02);
            assert_eq!(params.editable_curve_snapshot(), curve);
            send_key(app, fixture.window, &gui, "z", 6, COMMAND);
            assert_eq!(params.effects()[0], 0.);
            assert_eq!(params.effects()[index], 1.);
            send_key(app, fixture.window, &gui, "z", 6, COMMAND | SHIFT);
            assert_eq!(params.effects()[0], 1.);
        }
        click_control(fixture, "dual-solo-low", 0);
        assert_eq!(params.effects()[5], 1.);
        click_control(fixture, "dual-solo-low", 0);
        assert_eq!(params.effects()[5], 0.);
        click_control(fixture, "dual-solo-high", 0);
        assert_eq!(params.effects()[6], 1.);
        click_control(fixture, "dual-solo-high", 0);
        assert_eq!(params.effects()[6], 0.);
        let slope = params.effects()[2];
        click_control(fixture, "dual-slope", 0);
        assert_ne!(params.effects()[2], slope);

        // A grab slightly off the line must preserve its pointer offset.
        params.set_effect(0, 0.0);
        params.set_effect(1, 200.0);
        pump_appkit(app, &gui, 0.04);
        let [marker_x, marker_y, marker_width, marker_height] =
            pmpx::gui_gpui::screenshot_bounds("crossover-marker");
        let x = f64::from(marker_x + marker_width * 0.5 + 4.0);
        let y = f64::from(marker_y + marker_height * 0.25);
        let [_, _, plot_width, _] = pmpx::gui_gpui::screenshot_bounds("frequency-plot");
        let bright_gray =
            |pixel: &[u8]| pixel[0] > 180 && pixel[0] == pixel[1] && pixel[1] == pixel[2];
        let idle = count_region_pixels(&gui, "frequency-plot", bright_gray);
        send_mouse_move(fixture.window, x, y);
        pump_appkit(app, &gui, 0.04);
        assert!(
            count_region_pixels(&gui, "frequency-plot", bright_gray) > idle + 40,
            "hover should brighten the crossover line"
        );
        send_click(fixture.window, x, y, 0);
        assert_eq!(params.effects()[..2], [0.0, 200.0]);
        let curve = params.editable_curve_snapshot();
        send_mouse_down(fixture.window, x, y, 0);
        let end_x = x + f64::from((plot_width - 17.0) / 3.0);
        send_mouse_dragged(fixture.window, end_x, y, 0);
        send_mouse_up(fixture.window, end_x, y, 0);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.effects()[0], 1.0);
        assert!((params.effects()[1] - 2000.0).abs() < 1.0);
        assert_eq!(params.editable_curve_snapshot(), curve);
        send_key(app, fixture.window, &gui, "z", 6, COMMAND);
        assert_eq!(params.effects()[..2], [0.0, 200.0]);
        send_key(app, fixture.window, &gui, "z", 6, COMMAND | SHIFT);
        assert_eq!(params.effects()[0], 1.0);

        // Open COMP with FILTER still open. The editor keeps its width and
        // shifts to make room on the left.
        click_control(fixture, "compressor-panel", 0);
        pump_appkit(app, &gui, 0.06);
        assert_eq!(params.compressor()[0], 1.0);
        assert_eq!(params.effects()[0], 1.0);
        fixture.sync_host_size(OUTPUT_WIDTH + 2 * PANEL_WIDTH);
        let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds("curve-plot");
        assert!((x - (base_plot[0] + PANEL_WIDTH as f32)).abs() < 0.01);
        assert_eq!([y, w, h], base_plot[1..]);
        click_control(fixture, "filter-panel", 0);
        pump_appkit(app, &gui, 0.06);
        assert_eq!(params.effects()[0], 0.0);
        assert_eq!(params.compressor()[0], 1.0);
        fixture.sync_host_size(OUTPUT_WIDTH + PANEL_WIDTH);
        assert_eq!(
            pmpx::gui_gpui::screenshot_bounds("curve-plot"),
            [x, y, w, h]
        );
        assert!(pmpx::gui_gpui::screenshot_bounds_if_painted("compressor-16").is_none());
        for index in (0..20).filter(|&index| index != 16) {
            let key = format!("compressor-{index}");
            wait_for_control(app, &gui, &key);
            let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds(&key);
            assert!(
                x >= 0.
                    && y >= 0.
                    && w > 0.
                    && h > 0.
                    && x + w <= PANEL_WIDTH as f32
                    && y + h <= OUTPUT_HEIGHT as f32,
                "{key} outside editor: {x},{y},{w},{h}"
            );
        }
        let [release_x, release_y, release_w, release_h] =
            pmpx::gui_gpui::screenshot_bounds("compressor-3");
        let [auto_x, auto_y, auto_w, _] = pmpx::gui_gpui::screenshot_bounds("compressor-17");
        assert!(((auto_x + auto_w * 0.5) - (release_x + release_w * 0.5)).abs() < 1.);
        assert!((0.0..=4.0).contains(&(auto_y - (release_y + release_h))));
        click_control(fixture, "compressor-17", 0);
        assert_eq!(params.compressor()[17], 1.0);
        click_control(fixture, "compressor-17", 0);
        assert_eq!(params.compressor()[17], 0.0);
        click_control(fixture, "compressor-0", 0);
        assert_eq!(params.compressor()[0], 0.0);
        click_control(fixture, "compressor-0", 0);
        assert_eq!(params.compressor()[0], 1.0);
        let coral = |pixel: &[u8]| pixel[0] > 185 && pixel[1] < 145 && pixel[2] < 115;
        let blue = |pixel: &[u8]| pixel[2] > 137 && pixel[2].saturating_sub(pixel[0]) > 45;
        pump_appkit(app, &gui, 0.04);
        let coral_before = count_plot_pixels(&gui, coral);
        let blue_before = count_plot_pixels(&gui, blue);
        pmpx::gui_gpui::seed_screenshot_waveform(&status, &params);
        pump_appkit(app, &gui, 0.04);
        assert!(
            count_plot_pixels(&gui, coral) > coral_before + 40,
            "the live compressor trace should render before capture"
        );
        click_control(fixture, "sidechain-waveform", 0);
        pump_appkit(app, &gui, 0.04);
        assert!(
            count_plot_pixels(&gui, blue) > blue_before + 40,
            "clicking SIDECHAIN should show the overlay in the plot"
        );
        send_key(app, fixture.window, &gui, "\r", 36, 0);
        assert!(
            count_plot_pixels(&gui, blue) <= blue_before + 10,
            "Enter should hide the focused sidechain overlay"
        );

        let before_threshold = params.compressor()[1];
        let (x, y) = center("compressor-1");
        send_mouse_down(fixture.window, x, y, 0);
        send_mouse_dragged(fixture.window, x, y - 40.0, 0);
        send_mouse_up(fixture.window, x, y - 40.0, 0);
        pump_appkit(app, &gui, 0.04);
        assert!(
            params.compressor()[1] > before_threshold,
            "compressor threshold must track native drag"
        );
        let (x, y) = center("compressor-19");
        send_mouse_down(fixture.window, x, y, 0);
        send_mouse_dragged(fixture.window, x, y - 36.0, 0);
        send_mouse_up(fixture.window, x, y - 36.0, 0);
        assert!((params.compressor()[19] - 30.0).abs() < 0.01);
        params.set_delay_beats(0.0);
        let previous_curve = params.editable_curve_snapshot();
        let [x, _, w, _] = pmpx::gui_gpui::screenshot_bounds("compressor-capture");
        assert!(
            x + w <= PANEL_WIDTH as f32,
            "Capture belongs in the compressor drawer"
        );
        click_control(fixture, "compressor-capture", 0);
        pump_appkit(app, &gui, 0.04);
        assert!(pmpx::gui_gpui::complete_screenshot_compressor_capture(
            status.as_ref(),
            0.5
        ));
        pump_appkit(app, &gui, 0.1);
        assert_eq!(
            params.compressor()[0],
            0.0,
            "a completed capture must turn compression off"
        );
        let captured = params.editable_curve_snapshot();
        assert!(captured.smooth_bezier);
        assert!((captured.nodes[0].y - 0.5).abs() < 0.001);
        capture_frame(&gui, "captured Bézier envelope");
        send_key(app, fixture.window, &gui, "z", 6, COMMAND);
        assert_eq!(
            params.editable_curve_snapshot(),
            previous_curve,
            "capture must be undoable"
        );
        assert_eq!(params.compressor()[0], 1.0);
        click_control(fixture, "compressor-panel", 0);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.compressor()[0], 0.0);

        fixture.sync_host_size(OUTPUT_WIDTH);
        assert_eq!(pmpx::gui_gpui::screenshot_bounds("curve-plot"), base_plot);

        // Mix/Output are retained knobs with independent editable values.
        type_value(app, fixture, &gui, "value-Mix", "62");
        assert!((params.mix() - 0.62).abs() < 0.001);
        type_value(app, fixture, &gui, "value-OutputGain", "-3");
        assert!((params.output_gain_db() + 3.).abs() < 0.001);
        click_control(fixture, "timing-mode", 0);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.timing_mode(), 1);
        click_control(fixture, "timing-value", 0);
        pump_appkit(app, &gui, 0.04);
        wait_for_control(app, &gui, "timing-unit-ms");
        click_control(fixture, "timing-unit-ms", 0);
        pump_appkit(app, &gui, 0.04);
        wait_for_control(app, &gui, "value-FreeRate");
        type_value(app, fixture, &gui, "value-FreeRate", "500 ms");
        assert!((params.free_rate_hz() - 2.).abs() < 0.001);
        click_control(fixture, "timing-mode", 0);
        pump_appkit(app, &gui, 0.04);

        // Curve hit testing uses the same painted plot as the envelope.
        let origin = params.editable_curve_snapshot();
        let slot = "curve-slot-0";
        let amber = |pixel: &[u8]| pixel[0] > 218 && pixel[1] > 165 && pixel[2] < 140;
        click_control(fixture, slot, COMMAND);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.global_curve_slot_curve(0), Some(origin.clone()));
        assert_eq!(count_region_pixels(&gui, slot, amber), 0);
        let node = origin.nodes[1];
        let [left, top, width, height] = pmpx::gui_gpui::screenshot_bounds("curve-plot");
        let x = f64::from(left + node.x * (width - 1.));
        let y = f64::from(top + (1. - node.y) * (height - 1.));
        send_mouse_move_with_modifiers(fixture.window, x, y, 0);
        send_mouse_down(fixture.window, x, y, 0);
        send_mouse_dragged(fixture.window, x + 8., y - 12., 0);
        send_mouse_up(fixture.window, x + 8., y - 12., 0);
        assert_ne!(params.editable_curve_snapshot(), origin);
        pump_appkit(app, &gui, 0.04);
        assert!(count_region_pixels(&gui, slot, amber) > 80);
        assert_eq!(count_region_pixels(&gui, "curve-slot-1", amber), 0);
        assert_eq!(params.global_curve_slot_curve(0), Some(origin.clone()));
        send_key(app, fixture.window, &gui, "z", 6, COMMAND);
        assert_eq!(params.editable_curve_snapshot(), origin);
        assert_eq!(count_region_pixels(&gui, slot, amber), 0);
        // Exercise the native paint path while retaining authored data in undo.
        let paint_x = f64::from(left + width * 0.65);
        let paint_y = f64::from(top + height * 0.5);
        send_secondary_mouse_down(fixture.window, paint_x, paint_y, 0);
        send_secondary_mouse_dragged(fixture.window, paint_x + 12., paint_y - 10., 0);
        send_secondary_mouse_up(fixture.window, paint_x + 12., paint_y - 10., 0);
        let painted = params.editable_curve_snapshot();
        assert_ne!(painted, origin);
        click_control(fixture, slot, COMMAND);
        pump_appkit(app, &gui, 0.04);
        assert_eq!(params.global_curve_slot_curve(0), Some(painted));
        assert_eq!(count_region_pixels(&gui, slot, amber), 0);
        send_key(app, fixture.window, &gui, "z", 6, COMMAND);
        assert_eq!(params.editable_curve_snapshot(), origin);
        let before = params.active_sound();
        click_control(fixture, "sound-b", 0);
        assert_ne!(params.active_sound(), before);
        click_control(fixture, "sound-a", 0);
        assert_eq!(params.active_sound(), before);

        click_control(fixture, "bypass", 0);
        assert!(params.bypassed());
        send_repeated_key(app, fixture.window, &gui, " ", 49, 0);
        assert!(
            !params.bypassed(),
            "a repeated Space press toggles bypass once"
        );
        send_key(app, fixture.window, &gui, "\r", 36, 0);
        assert!(params.bypassed());
        gui.close();
        gui.set_parent_raw(fixture.parent_handle());
        assert!(gui.open(), "Pump editor should reopen");
        pump_appkit(app, &gui, 0.05);
        assert_delay_text_edit(app, fixture, &gui, "6");
        assert_eq!(params.delay_beats(), 6);
        capture_frame(&gui, "current controls and reopened editor");
        gui.close();
    }

    #[cfg(feature = "vst3")]
    unsafe fn exercise_vst3_window_anchor() {
        use cocoa::appkit::NSScreen;
        use std::cell::Cell;
        use std::rc::Rc;
        use toybox::vst3::prelude::Steinberg::*;
        use toybox::vst3::prelude::{Class, ComPtr, ComRef, ComWrapper};

        struct HostResize {
            window: id,
            moves_left: Rc<Cell<bool>>,
        }

        impl Class for HostResize {
            type Interfaces = (IPlugFrame,);
        }

        impl IPlugFrameTrait for HostResize {
            unsafe fn resizeView(&self, view: *mut IPlugView, rect: *mut ViewRect) -> tresult {
                let view = ComRef::from_raw(view).expect("VST3 view");
                let requested = *rect;
                let before = NSWindow::frame(self.window);
                let width = f64::from(requested.right - requested.left);
                self.window.setContentSize_(NSSize::new(
                    width,
                    f64::from(requested.bottom - requested.top),
                ));
                // Some hosts move the outer window themselves before calling onSize.
                if self.moves_left.get() {
                    let after = NSWindow::frame(self.window);
                    self.window.setFrameOrigin_(NSPoint::new(
                        before.origin.x + before.size.width - after.size.width,
                        before.origin.y,
                    ));
                }
                view.onSize(rect)
            }
        }

        let app = NSApp();
        let fixture = NativeFixture::new();
        let screen = fixture.window.screen();
        assert!(!screen.is_null(), "native host window has a screen");
        let visible = screen.visibleFrame();
        fixture.window.setLevel_(3);
        fixture.window.setFrameOrigin_(NSPoint::new(
            visible.origin.x + f64::from(PANEL_WIDTH) + 24.0,
            visible.origin.y + 30.0,
        ));
        let moves_left = Rc::new(Cell::new(false));
        let frame = ComWrapper::new(HostResize {
            window: fixture.window,
            moves_left: Rc::clone(&moves_left),
        })
        .to_com_ptr::<IPlugFrame>()
        .expect("VST3 host frame");
        let view = ComPtr::from_raw(pmpx::new_screenshot_vst3_editor()).expect("VST3 editor");
        assert_eq!(view.setFrame(frame.as_ptr()), kResultOk);
        assert_eq!(
            view.attached(fixture.view.cast(), kPlatformTypeNSView),
            kResultOk
        );
        pump_appkit_events(app, 0.08, || {});

        let plot_on_screen = || {
            let [x, y, w, h] = pmpx::gui_gpui::screenshot_bounds("curve-plot");
            let window_point = NSRect::new(
                NSPoint::new(f64::from(x), f64::from(OUTPUT_HEIGHT) - f64::from(y)),
                NSSize::new(0.0, 0.0),
            );
            let point = fixture.window.convertRectToScreen_(window_point).origin;
            [point.x, point.y, f64::from(w), f64::from(h)]
        };
        let assert_anchored = |base: [f64; 4], width: u32| {
            let actual = plot_on_screen();
            assert!(
                actual.iter().zip(base).all(|(&a, b)| (a - b).abs() < 0.02),
                "curve editor moved on screen from {base:?} to {actual:?}"
            );
            let dimensions = NSWindow::frame(fixture.window).size;
            assert_eq!(
                (dimensions.width, dimensions.height),
                (f64::from(width), f64::from(OUTPUT_HEIGHT))
            );
        };

        let original = plot_on_screen();
        for host_moves_left in [false, true] {
            moves_left.set(host_moves_left);
            click_control(&fixture, "compressor-panel", 0);
            pump_appkit_events(app, 0.08, || {});
            assert_anchored(original, OUTPUT_WIDTH + PANEL_WIDTH);
            click_control(&fixture, "compressor-panel", 0);
            pump_appkit_events(app, 0.08, || {});
            assert_anchored(original, OUTPUT_WIDTH);
        }

        // Where a left expansion will run off-screen, the compressor stays
        // overlaid and the filter can still dock independently on the right.
        moves_left.set(false);
        fixture.window.setFrameOrigin_(NSPoint::new(
            visible.origin.x + 24.0,
            visible.origin.y + 30.0,
        ));
        let edge = plot_on_screen();
        click_control(&fixture, "compressor-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(edge, OUTPUT_WIDTH);
        click_control(&fixture, "filter-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(edge, OUTPUT_WIDTH + PANEL_WIDTH);
        let [filter_x, _, _, _] = pmpx::gui_gpui::screenshot_bounds("frequency-plot");
        assert!(filter_x >= OUTPUT_WIDTH as f32);
        click_control(&fixture, "filter-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(edge, OUTPUT_WIDTH);
        let [_, capture_y, _, capture_h] = pmpx::gui_gpui::screenshot_bounds("compressor-capture");
        assert!(capture_y + capture_h < center("compressor-panel").1 as f32);
        click_control(&fixture, "compressor-panel", 0);
        pump_appkit_events(app, 0.08, || {});

        // A second click on the edge icon must also dismiss the compressor
        // overlay. The next click docks it after we make room on the left.
        fixture.window.setFrameOrigin_(NSPoint::new(
            visible.origin.x + f64::from(PANEL_WIDTH) + 24.0,
            visible.origin.y + 30.0,
        ));
        click_control(&fixture, "compressor-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(original, OUTPUT_WIDTH + PANEL_WIDTH);
        click_control(&fixture, "compressor-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(original, OUTPUT_WIDTH);

        // No host frame means the right-hand panel must open as an overlay,
        // with the same icon available to close it.
        assert_eq!(view.setFrame(std::ptr::null_mut()), kResultOk);
        click_control(&fixture, "filter-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(original, OUTPUT_WIDTH);
        let [filter_x, _, _, _] = pmpx::gui_gpui::screenshot_bounds("frequency-plot");
        assert!(filter_x > 0.0 && filter_x < OUTPUT_WIDTH as f32);
        click_control(&fixture, "filter-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_eq!(view.setFrame(frame.as_ptr()), kResultOk);
        click_control(&fixture, "filter-panel", 0);
        pump_appkit_events(app, 0.08, || {});
        assert_anchored(original, OUTPUT_WIDTH + PANEL_WIDTH);
        assert_eq!(view.removed(), kResultOk);
    }

    pub fn run() {
        unsafe {
            let pool = NSAutoreleasePool::new(nil);
            let _curve_slot_sandbox = CurveSlotSandbox::new();
            let fixture = NativeFixture::new();
            exercise_native_input(&fixture);
            drop(fixture);
            #[cfg(feature = "vst3")]
            exercise_vst3_window_anchor();
            pool.drain();
        }
    }
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Pump GPUI input fixture requires macOS");
    std::process::exit(1);
}

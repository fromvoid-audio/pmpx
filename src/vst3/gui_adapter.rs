use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::gui::gui_gpui::{ResizeDisposition, ResizeRequester};

pub(super) struct Vst3HostParamEditSink {
    pub(super) shared: Arc<PumpVst3Shared>,
}

fn vst3_param_id_for_gui(param_id: toybox::clack_plugin::utils::ClapId) -> ParamID {
    crate::params::vst3_id_from_clap_param_id(param_id).unwrap_or(param_id.get())
}

impl crate::gui::HostParamEditSink for Vst3HostParamEditSink {
    fn edit(
        &self,
        config: &toybox::clap::automation::AutomationConfig,
        param_id: toybox::clack_plugin::utils::ClapId,
        value: f64,
    ) -> bool {
        if !config.is_enabled(param_id) {
            return false;
        }
        let vst3_id = vst3_param_id_for_gui(param_id);
        let Some(normalized) = normalized_from_vst3_plain_value(vst3_id, value) else {
            return false;
        };
        let Ok(handler) = self.shared.component_handler.lock() else {
            return false;
        };
        let Some(handler) = handler.as_ref() else {
            return false;
        };
        unsafe {
            if handler.beginEdit(vst3_id) != kResultOk {
                return false;
            }
            let performed = handler.performEdit(vst3_id, normalized) == kResultOk;
            let _ended = handler.endEdit(vst3_id);
            performed
        }
    }

    fn gesture_started(
        &self,
        config: &toybox::clap::automation::AutomationConfig,
        param_id: toybox::clack_plugin::utils::ClapId,
    ) -> bool {
        if !config.is_enabled(param_id) {
            return false;
        }
        let vst3_id = vst3_param_id_for_gui(param_id);
        let Ok(handler) = self.shared.component_handler.lock() else {
            return false;
        };
        let Some(handler) = handler.as_ref() else {
            return false;
        };
        unsafe { handler.beginEdit(vst3_id) == kResultOk }
    }

    fn gesture_value(
        &self,
        config: &toybox::clap::automation::AutomationConfig,
        param_id: toybox::clack_plugin::utils::ClapId,
        value: f64,
    ) -> bool {
        if !config.is_enabled(param_id) {
            return false;
        }
        let vst3_id = vst3_param_id_for_gui(param_id);
        let Some(normalized) = normalized_from_vst3_plain_value(vst3_id, value) else {
            return false;
        };
        let Ok(handler) = self.shared.component_handler.lock() else {
            return false;
        };
        let Some(handler) = handler.as_ref() else {
            return false;
        };
        unsafe { handler.performEdit(vst3_id, normalized) == kResultOk }
    }

    fn gesture_ended(
        &self,
        config: &toybox::clap::automation::AutomationConfig,
        param_id: toybox::clack_plugin::utils::ClapId,
    ) -> bool {
        if !config.is_enabled(param_id) {
            return false;
        }
        let vst3_id = vst3_param_id_for_gui(param_id);
        let Ok(handler) = self.shared.component_handler.lock() else {
            return false;
        };
        let Some(handler) = handler.as_ref() else {
            return false;
        };
        unsafe { handler.endEdit(vst3_id) == kResultOk }
    }
}

pub(super) struct PumpVst3GuiAdapter {
    gpui_gui: toybox::gpui_gui::GpuiHostedGui,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ShortcutModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl PumpVst3GuiAdapter {
    fn with_resize(shared: Arc<PumpVst3Shared>, resize: ResizeRequester) -> Self {
        let gpui_gui = crate::gui::gui_gpui::new_gui_with_edit_sink(
            Arc::clone(&shared.params),
            Arc::clone(&shared.status),
            Arc::new(Vst3HostParamEditSink {
                shared: Arc::clone(&shared),
            }),
            resize,
        );
        Self { gpui_gui }
    }

    /// Decode VST3 modifier bit flags into Pump shortcut modifiers.
    ///
    /// Steinberg hosts commonly encode bitflags with shift/alt/control in the
    /// low bits. We accept both control-style bits to remain host-tolerant.
    #[cfg(test)]
    pub(super) fn shortcut_modifiers(modifiers: int16) -> ShortcutModifiers {
        let bits = modifiers as u16;
        ShortcutModifiers {
            shift: (bits & 0b0001) != 0,
            alt: (bits & 0b0010) != 0,
            ctrl: (bits & 0b0100) != 0 || (bits & 0b1000) != 0,
        }
    }

    /// Resolve a VST3 key event into one character/control input.
    #[cfg(test)]
    pub(super) fn key_char(key: char16, key_code: int16) -> Option<char> {
        toybox::vst3::gui::vst3_key_down_to_input_char(key, key_code)
    }
}

impl Vst3HostedGui for PumpVst3GuiAdapter {
    fn set_parent_raw(&mut self, parent: toybox::raw_window_handle::RawWindowHandle) {
        self.gpui_gui.set_parent_raw(parent);
    }

    fn open(&mut self) -> bool {
        self.gpui_gui.open()
    }

    fn close(&mut self) {
        self.gpui_gui.close();
    }

    fn last_size(&self) -> Option<(u32, u32)> {
        self.gpui_gui.last_size()
    }

    fn show(&self) -> bool {
        self.gpui_gui.show()
    }

    fn set_callback_keyboard_mode(&mut self, callback_only: bool) {
        self.gpui_gui.set_callback_keyboard_mode(callback_only);
    }

    fn host_size_from_logical(&self, width: u32, height: u32) -> (u32, u32) {
        self.gpui_gui.host_size_from_logical(width, height)
    }

    fn logical_size_from_host(&self, width: u32, height: u32) -> (u32, u32) {
        self.gpui_gui.logical_size_from_host(width, height)
    }

    fn request_resize(&self, width: u32, height: u32) {
        self.gpui_gui.request_resize(width, height);
    }

    fn on_key_down(&self, key: char16, key_code: int16, modifiers: int16) -> bool {
        self.gpui_gui.on_key_down(key, key_code, modifiers)
    }

    fn on_key_up(&self, key: char16, key_code: int16, modifiers: int16) -> bool {
        self.gpui_gui.on_key_up(key, key_code, modifiers)
    }

    fn on_focus(&self, focused: bool) -> bool {
        self.gpui_gui.on_focus(focused)
    }
}

#[derive(Default)]
struct ResizeTarget {
    view: Cell<*mut IPlugView>,
    frame: RefCell<Option<ComPtr<IPlugFrame>>>,
    attached: Cell<bool>,
}

impl ResizeTarget {
    fn request(&self, width: u32, height: u32) -> ResizeDisposition {
        if !self.attached.get() {
            return ResizeDisposition::Rejected;
        }
        // SAFETY: The view pointer belongs to the wrapper that owns this target.
        // Its destructor clears the pointer before the allocation is released.
        let Some(view) = (unsafe { ComRef::from_raw(self.view.get()) }) else {
            return ResizeDisposition::Rejected;
        };
        let view = view.to_com_ptr();
        let Some(frame) = self.frame.borrow().clone() else {
            return ResizeDisposition::Rejected;
        };
        let mut rect = view_rect(width as i32, height as i32);
        // No editor or frame borrow is held while the host calls onSize.
        if unsafe { frame.resizeView(view.as_ptr(), &mut rect) } == kResultOk {
            ResizeDisposition::HostWillResize
        } else {
            ResizeDisposition::Rejected
        }
    }
}

pub(super) struct PumpVst3EditorView {
    inner: HostedVst3View<PumpVst3GuiAdapter>,
    resize: Rc<ResizeTarget>,
}

impl PumpVst3EditorView {
    pub(super) fn create(shared: Arc<PumpVst3Shared>) -> *mut IPlugView {
        Self::with_resize_target(shared, Rc::new(ResizeTarget::default()))
            .map_or(ptr::null_mut(), ComPtr::into_raw)
    }

    fn with_resize_target(
        shared: Arc<PumpVst3Shared>,
        resize: Rc<ResizeTarget>,
    ) -> Option<ComPtr<IPlugView>> {
        let requester = Rc::clone(&resize);
        let adapter = PumpVst3GuiAdapter::with_resize(
            shared,
            Rc::new(move |width, height| requester.request(width, height)),
        );
        let (width, height) = preferred_window_size();
        let inner = HostedVst3View::new(adapter, width, height)
            .with_size_bounds(
                MIN_WINDOW_WIDTH,
                MIN_WINDOW_HEIGHT,
                crate::gui::MAX_EXPANDED_WIDTH,
                MAX_WINDOW_HEIGHT,
            )
            .preserve_aspect_ratio(false)
            .enforce_minimum_size(true);
        let view = ComWrapper::new(Self {
            inner,
            resize: Rc::clone(&resize),
        })
        .to_com_ptr::<IPlugView>()?;
        resize.view.set(view.as_ptr());
        Some(view)
    }
}

impl Drop for PumpVst3EditorView {
    fn drop(&mut self) {
        self.resize.view.set(ptr::null_mut());
        let frame = self.resize.frame.borrow_mut().take();
        drop(frame);
    }
}

impl Class for PumpVst3EditorView {
    type Interfaces = (IPlugView,);
}

impl IPlugViewTrait for PumpVst3EditorView {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        unsafe { self.inner.isPlatformTypeSupported(platform) }
    }

    unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
        let result = unsafe { self.inner.attached(parent, platform) };
        self.resize.attached.set(result == kResultOk);
        result
    }

    unsafe fn removed(&self) -> tresult {
        self.resize.attached.set(false);
        let frame = self.resize.frame.borrow_mut().take();
        drop(frame);
        unsafe { self.inner.removed() }
    }

    unsafe fn onWheel(&self, distance: f32) -> tresult {
        unsafe { self.inner.onWheel(distance) }
    }

    unsafe fn onKeyDown(&self, key: char16, code: int16, modifiers: int16) -> tresult {
        unsafe { self.inner.onKeyDown(key, code, modifiers) }
    }

    unsafe fn onKeyUp(&self, key: char16, code: int16, modifiers: int16) -> tresult {
        unsafe { self.inner.onKeyUp(key, code, modifiers) }
    }

    unsafe fn getSize(&self, rect: *mut ViewRect) -> tresult {
        unsafe { self.inner.getSize(rect) }
    }

    unsafe fn onSize(&self, rect: *mut ViewRect) -> tresult {
        unsafe { self.inner.onSize(rect) }
    }

    unsafe fn onFocus(&self, focused: TBool) -> tresult {
        unsafe { self.inner.onFocus(focused) }
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        // SAFETY: VST3 passes a borrowed frame. Retain it until detachment
        // or the next setFrame call.
        let frame = unsafe { ComRef::from_raw(frame) }.map(|frame| frame.to_com_ptr());
        let previous = self.resize.frame.replace(frame);
        drop(previous);
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        unsafe { self.inner.canResize() }
    }

    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        unsafe { self.inner.checkSizeConstraint(rect) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ResizeFrame {
        accepts_resize: Rc<Cell<bool>>,
    }

    impl Class for ResizeFrame {
        type Interfaces = (IPlugFrame,);
    }

    impl IPlugFrameTrait for ResizeFrame {
        unsafe fn resizeView(&self, view: *mut IPlugView, rect: *mut ViewRect) -> tresult {
            if !self.accepts_resize.get() {
                return kResultFalse;
            }
            let view = unsafe { ComRef::from_raw(view) }.expect("requesting plug-in view");
            unsafe { view.onSize(rect) }
        }
    }

    #[test]
    fn panel_request_round_trips_through_the_vst3_host_frame() {
        let target = Rc::new(ResizeTarget::default());
        let view = PumpVst3EditorView::with_resize_target(
            Arc::new(PumpVst3Shared::new()),
            Rc::clone(&target),
        )
        .expect("editor view");

        let accepts_resize = Rc::new(Cell::new(true));
        let frame = ComWrapper::new(ResizeFrame {
            accepts_resize: Rc::clone(&accepts_resize),
        })
        .to_com_ptr::<IPlugFrame>()
        .expect("host frame");
        assert_eq!(unsafe { view.setFrame(frame.as_ptr()) }, kResultOk);
        drop(frame);
        // The native fixture tests attachment and layout. Here the frame
        // synchronously calls back into the real IPlugView just as a host can.
        target.attached.set(true);
        for width in [880, 1120, 880, 640] {
            assert!(matches!(
                target.request(width, 400),
                ResizeDisposition::HostWillResize
            ));
            let mut rect = view_rect(0, 0);
            assert_eq!(unsafe { view.getSize(&mut rect) }, kResultOk);
            assert_eq!((rect.right, rect.bottom), (width as i32, 400));
        }
        accepts_resize.set(false);
        assert!(matches!(
            target.request(880, 400),
            ResizeDisposition::Rejected
        ));
        let mut rect = view_rect(0, 0);
        assert_eq!(unsafe { view.getSize(&mut rect) }, kResultOk);
        assert_eq!((rect.right, rect.bottom), (640, 400));
        assert_eq!(unsafe { view.setFrame(ptr::null_mut()) }, kResultOk);
        assert!(matches!(
            target.request(880, 400),
            ResizeDisposition::Rejected
        ));
    }
}

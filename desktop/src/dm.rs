//! Precision touchpad through DirectManipulation, the way Chrome and Edge
//! get it: two-finger pan in any direction (plain wheel messages are
//! snapped to one axis by Windows) and the exact pinch scale.

use std::cell::RefCell;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::DirectManipulation::*;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Input::Pointer::GetPointerType;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{POINTER_INPUT_TYPE, PT_TOUCHPAD};
use windows_core::{implement, Ref, Result};

const DM_POINTERHITTEST: u32 = 0x0250;
/// Size of the virtual viewport DirectManipulation moves; reset after each gesture.
const VIEW: i32 = 1000;

pub enum Gesture {
    Pan(f64, f64),
    Zoom(f64),
}

struct Shared {
    events: Vec<Gesture>,
    /// Last content transform seen: scale, tx, ty.
    last: (f32, f32, f32),
    running: bool,
    /// A touchpad contact was handed over and is not finished yet.
    contact: bool,
    resetting: bool,
}

#[implement(IDirectManipulationViewportEventHandler)]
struct Handler {
    shared: Rc<RefCell<Shared>>,
}

impl IDirectManipulationViewportEventHandler_Impl for Handler_Impl {
    fn OnViewportStatusChanged(
        &self,
        viewport: Ref<IDirectManipulationViewport>,
        current: DIRECTMANIPULATION_STATUS,
        _previous: DIRECTMANIPULATION_STATUS,
    ) -> Result<()> {
        let needs_reset = {
            let mut s = self.shared.borrow_mut();
            s.running = current == DIRECTMANIPULATION_RUNNING;
            if current == DIRECTMANIPULATION_READY {
                s.contact = false;
            }
            current == DIRECTMANIPULATION_READY && s.last != (1.0, 0.0, 0.0)
        };
        if needs_reset {
            // Gesture over: snap the virtual content back to identity.
            self.shared.borrow_mut().resetting = true;
            if let Ok(v) = viewport.ok() {
                let _ = unsafe { v.ZoomToRect(0.0, 0.0, VIEW as f32, VIEW as f32, false) };
            }
            let mut s = self.shared.borrow_mut();
            s.resetting = false;
            s.last = (1.0, 0.0, 0.0);
        }
        Ok(())
    }

    fn OnViewportUpdated(&self, _viewport: Ref<IDirectManipulationViewport>) -> Result<()> {
        Ok(())
    }

    fn OnContentUpdated(&self, _viewport: Ref<IDirectManipulationViewport>, content: Ref<IDirectManipulationContent>) -> Result<()> {
        let c = content.ok()?;
        let mut m = [0f32; 6];
        unsafe { c.GetContentTransform(&mut m)? };
        let mut s = self.shared.borrow_mut();
        let (scale, tx, ty) = (m[0], m[4], m[5]);
        if s.resetting {
            s.last = (scale, tx, ty);
            return Ok(());
        }
        let (ls, lx, ly) = s.last;
        if (scale - ls).abs() > 1e-6 {
            s.events.push(Gesture::Zoom((scale / ls) as f64));
        } else if tx != lx || ty != ly {
            s.events.push(Gesture::Pan((tx - lx) as f64, (ty - ly) as f64));
        }
        s.last = (scale, tx, ty);
        Ok(())
    }
}

struct SubclassData {
    viewport: IDirectManipulationViewport,
    shared: Rc<RefCell<Shared>>,
}

unsafe extern "system" fn subclass(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    if msg == DM_POINTERHITTEST {
        let id = (wp.0 & 0xffff) as u32;
        let mut t = POINTER_INPUT_TYPE(0);
        if unsafe { GetPointerType(id, &mut t) }.is_ok() && t == PT_TOUCHPAD {
            let d = unsafe { &*(data as *const SubclassData) };
            if unsafe { d.viewport.SetContact(id) }.is_ok() {
                d.shared.borrow_mut().contact = true;
                // Wake the event loop so it starts pumping DirectManipulation.
                let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}

pub struct Dm {
    update: IDirectManipulationUpdateManager,
    shared: Rc<RefCell<Shared>>,
    _manager: IDirectManipulationManager,
    _viewport: IDirectManipulationViewport,
    _handler: IDirectManipulationViewportEventHandler,
}

impl Dm {
    pub fn new(hwnd: HWND) -> Result<Dm> {
        unsafe {
            let manager: IDirectManipulationManager = CoCreateInstance(&DirectManipulationManager, None, CLSCTX_INPROC_SERVER)?;
            let update: IDirectManipulationUpdateManager = manager.GetUpdateManager()?;
            let viewport: IDirectManipulationViewport = manager.CreateViewport(None, hwnd)?;
            viewport.ActivateConfiguration(DIRECTMANIPULATION_CONFIGURATION(
                DIRECTMANIPULATION_CONFIGURATION_INTERACTION.0
                    | DIRECTMANIPULATION_CONFIGURATION_TRANSLATION_X.0
                    | DIRECTMANIPULATION_CONFIGURATION_TRANSLATION_Y.0
                    | DIRECTMANIPULATION_CONFIGURATION_SCALING.0,
            ))?;
            viewport.SetViewportOptions(DIRECTMANIPULATION_VIEWPORT_OPTIONS_MANUALUPDATE)?;
            let shared = Rc::new(RefCell::new(Shared {
                events: Vec::new(),
                last: (1.0, 0.0, 0.0),
                running: false,
                contact: false,
                resetting: false,
            }));
            let handler: IDirectManipulationViewportEventHandler = Handler { shared: shared.clone() }.into();
            viewport.AddEventHandler(Some(hwnd), &handler)?;
            viewport.SetViewportRect(&RECT { left: 0, top: 0, right: VIEW, bottom: VIEW })?;
            manager.Activate(hwnd)?;
            viewport.Enable()?;
            let data = Box::into_raw(Box::new(SubclassData { viewport: viewport.clone(), shared: shared.clone() }));
            let _ = SetWindowSubclass(hwnd, Some(subclass), 1, data as usize);
            Ok(Dm { update, shared, _manager: manager, _viewport: viewport, _handler: handler })
        }
    }

    /// Pumps DirectManipulation and returns the gestures since the last call.
    pub fn poll(&self) -> Vec<Gesture> {
        if self.busy() {
            let _ = unsafe { self.update.Update(None) };
        }
        std::mem::take(&mut self.shared.borrow_mut().events)
    }

    /// True while a touchpad gesture is in progress (keep rendering frames).
    pub fn busy(&self) -> bool {
        let s = self.shared.borrow();
        s.running || s.contact
    }
}

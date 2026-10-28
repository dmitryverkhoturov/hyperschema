//! JNI bridge for the Android shell. All calls come from the UI thread;
//! the engine lives behind an opaque `long` handle.
#![cfg(target_os = "android")]

use hs_core::{bench, excalidraw, wgpu, Engine};
use jni::objects::{JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use ndk::native_window::NativeWindow;
use raw_window_handle::{AndroidDisplayHandle, AndroidNdkWindowHandle, RawDisplayHandle, RawWindowHandle};

struct App {
    instance: wgpu::Instance,
    engine: Engine,
    /// Keeps the ANativeWindow referenced while the surface uses it.
    _window: NativeWindow,
}

const FONTS: &[&str] = &[
    "/system/fonts/Roboto-Regular.ttf",
    "/system/fonts/RobotoStatic-Regular.ttf",
    "/system/fonts/NotoSans-Regular.ttf",
    "/system/fonts/DroidSans.ttf",
];

unsafe fn make_surface(instance: &wgpu::Instance, env: &JNIEnv, surface: &JObject) -> Option<(NativeWindow, wgpu::Surface<'static>)> {
    let window = NativeWindow::from_surface(env.get_raw() as _, surface.as_raw() as _)?;
    let handle = AndroidNdkWindowHandle::new(window.ptr().cast());
    let sf = instance
        .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Android(AndroidDisplayHandle::new())),
            raw_window_handle: RawWindowHandle::AndroidNdk(handle),
        })
        .ok()?;
    Some((window, sf))
}

fn app<'a>(h: jlong) -> &'a mut App {
    unsafe { &mut *(h as *mut App) }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_create(env: JNIEnv, _c: JClass, surface: JObject, w: jint, h: jint, boxes: jint) -> jlong {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let Some((window, sf)) = (unsafe { make_surface(&instance, &env, &surface) }) else { return 0 };
    let font = FONTS.iter().find_map(|p| std::fs::read(p).ok()).expect("no system font");
    let mut engine = pollster::block_on(Engine::new(&instance, sf, w as u32, h as u32, font, true));
    engine.set_scene(bench::generate(boxes.max(1) as usize));
    engine.fit();
    Box::into_raw(Box::new(App { instance, engine, _window: window })) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_attach(env: JNIEnv, _c: JClass, h: jlong, surface: JObject, w: jint, hgt: jint) {
    let a = app(h);
    if let Some((window, sf)) = unsafe { make_surface(&a.instance, &env, &surface) } {
        a.engine.attach_surface(sf, w as u32, hgt as u32);
        a._window = window;
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_detach(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).engine.detach_surface();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_resize(_env: JNIEnv, _c: JClass, h: jlong, w: jint, hgt: jint) {
    app(h).engine.resize(w as u32, hgt as u32);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_pan(_env: JNIEnv, _c: JClass, h: jlong, dx: jfloat, dy: jfloat) {
    app(h).engine.pan(dx as f64, dy as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_zoom(_env: JNIEnv, _c: JClass, h: jlong, f: jfloat, x: jfloat, y: jfloat) {
    app(h).engine.zoom_at(f as f64, x as f64, y as f64);
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_fit(_env: JNIEnv, _c: JClass, h: jlong) {
    app(h).engine.fit();
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_render(_env: JNIEnv, _c: JClass, h: jlong) -> jboolean {
    app(h).engine.render() as jboolean
}

/// Loads an Excalidraw scene; returns an error message or null.
#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_load(mut env: JNIEnv, _c: JClass, h: jlong, text: JString) -> jstring {
    let text: String = match env.get_string(&text) {
        Ok(s) => s.into(),
        Err(e) => return env.new_string(e.to_string()).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut()),
    };
    match excalidraw::load_str(&text) {
        Ok(scene) => {
            app(h).engine.set_scene(scene);
            app(h).engine.fit();
            std::ptr::null_mut()
        }
        Err(e) => env.new_string(e).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut()),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_hyperschema_app_Native_stats(env: JNIEnv, _c: JClass, h: jlong) -> jstring {
    let a = app(h);
    let s = a.engine.stats;
    let msg = format!(
        "{} эл. · кадр {:.1} мс · текст {:.1} мс · видно {}/{}/{} · {}",
        a.engine.scene().element_count(),
        s.frame_ms,
        s.text_ms,
        s.rects,
        s.lines,
        s.glyphs,
        a.engine.adapter_info
    );
    env.new_string(msg).map(|s| s.into_raw()).unwrap_or(std::ptr::null_mut())
}

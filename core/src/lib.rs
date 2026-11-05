//! Hyperschema core: board model, history, routing, scene, spatial index,
//! camera and GPU renderer. Platform shells (Windows, Android, ...) own
//! the window and input.

pub mod bench;
pub mod clock;
pub mod board;
pub mod doc;
pub mod editor;
pub mod engine;
pub mod excalidraw;
pub mod history;
pub mod i18n;
pub mod import;
pub mod route;
pub mod scene;
pub mod spatial;
pub mod sync;
pub mod library;
pub mod couch;
pub mod text;
pub mod touch;
pub mod trail;
pub mod view;

pub use engine::{Camera, Engine, Stats};
pub use wgpu;

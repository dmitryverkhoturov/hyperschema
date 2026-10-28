//! Hyperschema core: scene, spatial index, camera and GPU renderer.
//! Platform shells (Windows, Android, ...) own the window and input.

pub mod bench;
pub mod engine;
pub mod excalidraw;
pub mod scene;
pub mod spatial;
pub mod text;

pub use engine::{Camera, Engine, Stats};
pub use wgpu;

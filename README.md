# Hyperschema

Infinite whiteboard engine: Rust core (wgpu) + thin native shells.

- `core/` — scene, Morton quadtree index, camera, GPU renderer, glyph atlas, Excalidraw import.
- `desktop/` — Windows shell (winit). `hyperschema.exe [file.excalidraw(.md)]`, `--bench N`, `--autobench`, `--shot out.png --cam x,y,scale`.

Controls: touchpad two-finger scroll = pan, pinch / Ctrl+wheel = zoom, mouse wheel = zoom, drag = pan, F/Home = fit, B = benchmark.

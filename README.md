# Hyperschema

![A generated board of 106 000 concepts, zoomed into one project](docs/hero.png)

An infinite board for notes and diagrams, for Windows and Android.



https://github.com/user-attachments/assets/746a6310-09a8-4141-81c1-d461548bb848



## Features

- **Big boards.** 106 000 notes and 64 000 arrows at 57 fps.
- **Frames inside frames.** A note can hold other notes, each level smaller than
  the one above. You can zoom from the whole board down to one note.
- **Text stays visible.** When you zoom far out, text turns into soft bars.
- **Path mode.** Select a note and press *Path*. The notes it links to move next
  to it, so you can walk the board along the arrows. The app remembers each
  walk and brings it back next time.
- **Sync** between a laptop and a phone through your own CouchDB server.
- **Command line** for scripts and AI agents: `hyperschema add`, `link`,
  `move`, `show`.
- **Excalidraw import**, including Obsidian `.excalidraw.md` files.
- **Mouse, touchpad and touch.**



https://github.com/user-attachments/assets/30684fa1-ee5f-486b-bf0b-d7dc4734ec4a



## Screenshots

| Windows | Path mode |
|---|---|
| ![A board on Windows](docs/windows-board.png) | ![Path mode on Windows](docs/windows-path.png) |

<p align="center"><img src="docs/android.png" alt="Hyperschema on Android" width="320"></p>

## Sync

Without sync, boards stay on the device. To sync, you need a CouchDB 3 server
that all your devices can reach over HTTPS, a database (`hyperschema` by
default) and a user who can write to it.

On each device open menu → *Sync server…* and paste:

```json
{"url": "https://couch.example.com", "db": "hyperschema", "user": "…", "password": "…"}
```

The connection is stored only on the device. The command line uses the same
connection, so edits from scripts sync too.

Each board is an Automerge document. Devices only add their own changes to the
database, so there are no conflicts.

## Performance

Intel HD 4400 + GeForce 840M, 60 Hz screen. A generated board
with 6 levels of nesting, flown over for 14 seconds in real time.

| Notes | Arrows | Load | Average | p50 | p95 | p99 |
|---:|---:|---:|---:|---:|---:|---:|
| 106 000 | 64 000 | 13 s | 57 fps | 17.2 ms | 19.2 ms | 23.6 ms |

About 3% of frames take longer than 20 ms. These are moments when text is laid
out again at a new zoom level.

To repeat it:

```bash
cargo run --release -p hs-core --example gen_board -- 100000 big.hyperschema
hyperschema big.hyperschema --tour --hud
```

The results go to `bench-result.txt` and `bench-frames.csv`. The videos above
were rendered frame by frame with `--record`, so they look smooth on any
machine.

## Build

Requirements: Rust (stable). For Android also Android SDK, NDK 28 and JDK 17.

```bash
cargo build --profile dist -p hyperschema
```

The Windows build will be in `target/dist/hyperschema.exe`. For Android run
`bash tools/build-android.sh`, the APK will be in
`shell-android/app/build/outputs/apk/release/`.

## Project

- `core/`: board, editor, gestures, path mode, sync and the GPU renderer (wgpu).
- `desktop/`: Windows app and the command line.
- `android/`, `shell-android/`: Android app in Kotlin over the same core.
- `docs/demo/`: the board and script for the path mode video.

## License

[GPL-3.0](LICENSE). Text uses the system font: Segoe UI on Windows, Roboto or
Noto Sans on Android.

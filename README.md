# Verge

A fast, native, open non-linear video editor, written in Rust.

Verge is an editor and a compositor built on one engine: Premiere-style cutting
and After Effects-style compositing over a shared model of time, clips,
properties and render graphs. It is early — this repository is the foundation,
not the product — but the foundation is real and it runs.

**Native. Fast. Open. Extensible.**

## What works today

The first vertical slice is complete and tested end to end:

- Launch as a native desktop application
- Create a project; import video and audio
- Browse imported media with real probed metadata
- Place clips on a timeline across multiple video and audio tracks
- Move, trim and split clips, with snapping and frame-accurate editing
- Play the sequence, scrub, step by frame, stop and resume
- GPU compositing with transforms, opacity and alpha blending
- Undo and redo on every edit, with drags collapsed into single steps
- Save and reopen projects, with autosave and crash recovery
- A development performance overlay reporting real measurements

![The editor with a project open, playing](docs/images/editor.png)

The development overlay reports what the frame actually cost, broken down by
stage, so a regression is visible while editing rather than weeks later:

![The performance overlay](docs/images/performance-overlay.png)

## Building

Requires a Rust toolchain (1.95 or newer) and FFmpeg development libraries.

```sh
# Debian / Ubuntu
sudo apt install libavcodec-dev libavformat-dev libavutil-dev \
                 libswscale-dev libswresample-dev libasound2-dev pkg-config

# macOS
brew install ffmpeg pkg-config

cargo run --release
```

Open a project directly with `cargo run --release -- path/to/project.verge`.

## Testing

```sh
cargo test --workspace     # 308 tests
cargo bench                # measured, not estimated
```

The rendering tests run against a real GPU device and read pixels back. On a
machine with no GPU they use a software rasteriser rather than being skipped, so
the shader and blend state are always exercised.

Media tests run against committed fixtures in `testdata/`, where each video
frame is a solid colour encoding its own frame index. That lets a test assert
*which* frame a seek returned rather than merely that one came back — which is
how three real decoder bugs were caught.

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — the design and why it is that way
- [Project format](docs/PROJECT_FORMAT.md) — the `.verge` file
- [Benchmarks](docs/BENCHMARKS.md) — measured performance
- [Roadmap](docs/ROADMAP.md) — where this is going

## Layout

| Crate        | Responsibility                                            |
|--------------|-----------------------------------------------------------|
| `ve-time`    | Exact rational time, frame rates, timecode                |
| `ve-core`    | Project, sequence, track, clip, properties, keyframes     |
| `ve-project` | Versioned project file, atomic saves, autosave            |
| `ve-command` | Undoable editing commands                                 |
| `ve-media`   | FFmpeg decoding, frame cache, decode scheduling           |
| `ve-render`  | wgpu compositor                                           |
| `ve-engine`  | Playback clock, composition evaluation, audio mixing      |
| `ve-metrics` | Performance instrumentation                               |
| `ve-app`     | The editor shell                                          |

Dependencies point one way: `ve-app` knows about everything, `ve-time` knows
about nothing.

## Licence

MIT or Apache-2.0, at your option.

# Architecture

This document records the decisions that shape the codebase, and the reasoning
behind them. It is meant to be read before changing anything structural.

## The governing idea

> Own the editing architecture and the performance-critical systems. Use mature
> libraries for problems that are already solved.

Container parsing and video codecs are solved, with a decade of security
hardening behind them; Verge uses FFmpeg. Window creation and font shaping are
solved; Verge uses winit and egui. What Verge owns is everything above that
line: how time is represented, how edits are modelled, what is cached, how
obsolete work is cancelled, how frames reach the GPU, and how playback stays
attached to the clock.

## Crate graph

```
ve-app  ──────────────┬──────────┬──────────┬───────────┐
   │                  │          │          │           │
ve-engine ──┬─────────┤     ve-command  ve-project  ve-metrics
   │        │         │          │          │
ve-media  ve-render   │       ve-core ──────┘
   │        │         │          │
   └────────┴─────────┴──────► ve-time
```

Dependencies point one way. `ve-time` depends on nothing but `serde`;
`ve-core` is pure data with no I/O, no threads and no GPU. That is what makes
the edit model testable in isolation, and it is why the test suite can drive the
entire editor without opening a window.

## Time

**Timeline positions are never floating-point seconds.** They are integer ticks
on a fixed timebase of **282,240,000 ticks per second**.

That number is `2^10 · 3^2 · 5^4 · 7^2`, chosen so that every rate the editor
claims to support divides it exactly:

- Integer video rates: 24, 25, 30, 48, 50, 60, 120, 240
- NTSC rates: the `1001` denominators cancel because 24000, 30000, 60000 and
  120000 all divide the timebase
- Audio rates: 8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000,
  176400, 192000

At this resolution an `i64` spans about 1035 years.

Two consequences follow, and both are load-bearing:

1. **Frame positions are computed from the absolute frame index**, never by
   summing frame durations. `summing_frame_durations_matches_absolute_frame_positions`
   walks 100,000 frames at every supported rate and asserts the two agree.
2. **Audio sample indices convert exactly**, so the mixer and the video clock
   cannot drift apart.

Microseconds are the one common unit that does *not* divide the timebase
(`7056/25` ticks each), so `Ticks::from_micros` rounds and says so. That was
found by a test, not by inspection.

## The edit model

```
Project
 ├── MediaAsset[]        references files; never owns pixels or samples
 ├── Sequence[]
 │    ├── SequenceSettings   resolution, rate, sample rate, background
 │    ├── Track[]            video and audio lanes
 │    │    └── Clip[]        sorted, non-overlapping
 │    └── Marker[]
 └── IdAllocator         monotonic, never reuses
```

### Non-destructive editing

A `Clip` stores *where to look* in a source and *where to put it* on the
timeline: `source_in`, `timeline_start`, `duration`, `speed`. It never stores
media. Trimming narrows that window; it cannot destroy frames, and widening the
window again recovers them. `trimming_is_non_destructive_and_fully_reversible`
trims both ends and then restores the clip to byte-identical equality with the
original.

Splitting is the same guarantee in another form: the two halves cover the source
contiguously, so a split followed by deleting one half is exactly a trim.

### Track invariants

A track's clips are kept **sorted by start and free of overlaps** at all times.
Every mutating method either restores that or refuses the edit, and
`debug_assert!(self.invariants_hold())` follows every mutation, so a broken edit
path fails loudly in tests rather than corrupting a timeline quietly.

Holding the invariant is what lets lookups binary-search. `clips_in_range` costs
what is on screen, not what is in the project: **44 ns at 100 clips, 96 ns at
10,000**.

### IDs

Typed handles (`Id<T>`) over a monotonic counter persisted with the project.
Deleting never recycles an ID, so a stale reference resolves to "missing"
rather than to the wrong object. Undo deliberately does **not** rewind the
allocator: redo must reproduce identical IDs, and a new edit made after an undo
must not collide with a clip still sitting in the redo branch.

### One animation system

There is no per-property keyframe machinery. Transform, opacity, audio gain, pan
and arbitrary effect parameters all use `Property<T>`, which is a static value
plus an optional sorted keyframe list. Adding an animatable parameter means
implementing `Animatable` and nothing else.

Every easing mode is a cubic Bezier on the unit square evaluated by one solver,
so a graph editor can later promote any named preset to a free-form curve
without changing the representation.

Keyframe times are **clip-local**, which is what lets a clip be moved or rippled
without touching its animation — and what obliges `split_at` to rebase the
right-hand clip's keyframes.

## Commands and undo

Nothing mutates a project directly. The UI builds a `Command` and hands it to a
`History`. That buys three things at once: undo/redo on every operation rather
than as a retrofit, one audit point for dirty-marking and autosave, and a
scripting surface that already exists.

The contract is strict: `apply` captures what `undo` needs *before* mutating,
and both directions are all-or-nothing. A failed command leaves the project
untouched, so the history never pushes it.

**Gestures coalesce.** Dragging a clip produces one `MoveClip` per mouse move;
the history offers each to the entry on top, which absorbs the new destination
while keeping the original origin. A 40-step drag is one undo step that returns
to where the drag began. An explicit `break_merge` barrier closes the run on
mouse-up, so the next gesture is its own entry.

## Media

### Decode scheduling

Dragging a playhead across 4K footage can ask for a hundred frames a second.
Decoding all of them is impossible and pointless: by the time frame 40 is
decoded the user is at frame 900.

So each open file gets a worker thread with a **single-slot interactive queue**.
Posting a new interactive request overwrites whatever was waiting — and
overwriting *is* the cancellation. The worker always decodes the most recent
request rather than working through a backlog. Prefetch uses a separate bounded
FIFO and never pre-empts interactive work.

### Seeking

`frame_at` chooses between seeking and decoding forward based on distance:
seeking lands on a keyframe and has to decode forward anyway, so for the small
jumps that playback and fine scrubbing produce, decoding straight through is
faster and more accurate.

Deciding a frame is the last one at or before `t` requires seeing the *next*
frame, so the decoder overshoots by one and holds it. Two bugs lived here, both
caught by fixtures whose colours identify the frame: the overshoot was
originally discarded (so single-frame steps landed one frame late), and the
lookahead reset across calls (so a request one tick before a boundary returned
the next frame).

### Caching

The frame cache is bounded by **bytes, not entries**: a 4K frame is thirty times
a thumbnail, and a count-based budget would either waste memory or thrash. GPU
textures have their own separate budget, because VRAM is usually the scarcer
resource and evicting a texture only costs a re-upload, not a re-decode.

Frames are `Arc`-shared, so handing one to the cache, the uploader and the UI
costs three pointer copies.

## Rendering

A composition is a `RenderTarget` plus a back-to-front list of `Layer`s, each a
GPU texture with an affine transform and an opacity.

Building on the GPU from the start — rather than writing a CPU compositor and
porting later — is what keeps the interfaces about textures, render targets and
passes. Everything later attaches to that shape: blend modes become pipeline
variants, effects become passes between layer draws, masks become extra
bindings, and a nested composition becomes a layer whose texture is another
target's output.

The quad comes from the vertex index rather than a vertex buffer, and the
per-layer matrix is composed on the CPU, so the shader does no transform maths.
One uniform buffer addressed by dynamic offset serves every layer in a pass; the
texture bind group is built once at upload and cached with the texture. A
ten-layer composite is ten draws, one pass, one submit, and no per-frame
allocation.

### Colour

Output is **premultiplied**, paired with a `One / OneMinusSrcAlpha` blend. That
is what stops a nested composition double-applying its own alpha at every level.

Frames are uploaded as non-sRGB `Rgba8Unorm` and composited **non-linearly**,
matching the default behaviour of the established professional tools. An 8-bit
source blended in linear light shifts every crossfade and opacity ramp away from
what an editor coming from those tools expects. A linear-light mode belongs with
colour management, as an explicit project setting, not as a silent default.

## Playback

```
Sequence ──► evaluate() ──► Composition ──► decode requests
                                 │                │
                                 ▼                ▼
                          layer transforms   frame cache
                                 └───────┬────────┘
                                         ▼
                                    GPU compositor ──► preview texture
```

`evaluate(sequence, at)` is **pure**: no decoding, no GPU, no clock. Track
ordering, muting, soloing and animation are therefore answerable in a unit test.

**Position is derived from elapsed wall-clock time, never accumulated one frame
at a time.** Accumulating would tie playback speed to renderer speed, so a slow
frame would slow the audio down. Deriving means a slow frame drops a picture and
the sound keeps its pace — which is what every professional editor does.

The clock takes its time from an injected `TimeSource`, so tests drive the
transport from a fake clock: timing behaviour that can only be checked by
sleeping is behaviour that is not really being checked. One test plays a
simulated hour in 10,000 irregular steps and asserts the position has not
drifted by more than a single tick.

`update()` never blocks. A layer whose frame has not decoded is reported pending
and counted as a dropped frame.

## Audio

Mixing happens on an ordinary thread and writes into a single-producer,
single-consumer ring. The device callback does nothing but copy out, with
silence for any shortfall — no locks, no allocation, no decoding, because
anything that can block a real-time audio thread is heard as a click.

Pan uses a linear balance law: centre is unity on both sides, and panning
attenuates the far side without boosting the near one. Constant-power panning
keeps perceived loudness steadier but either drops the centre by 3 dB or boosts
the extremes by 3 dB, and a pan control that quietly changes the level of
centred audio, or that can push a mix into clipping, is the worse surprise.

Clipping is counted and reported rather than swallowed.

## The project file

See [PROJECT_FORMAT.md](PROJECT_FORMAT.md). In short: pretty-printed JSON behind
a versioned envelope, media referenced and never embedded, and saves that are
atomic — temp file, fsync, rename, with the previous version rotated to `.bak`.

## Instrumentation

Speed is a product requirement, which means it has to be a number rather than an
impression. Spans, counters and gauges are always on rather than behind a
feature flag, because a measurement you have to recompile to get is one you will
not take. Recording a span costs about a hundred nanoseconds against a 16 ms
frame budget.

The overlay reports **p95 alongside the mean**: a mean of 5 ms with a p95 of
40 ms is a stutter the user sees, and the mean alone hides it.

## The interface

egui, on eframe's wgpu backend, chosen for three reasons that matter here:

1. **Direct wgpu access.** The compositor shares the interface's device, so a
   decoded frame is uploaded once and both draw from the same texture. A
   separate device would mean a copy across the bus every frame.
2. **Painting, not widgets.** A sequence can hold thousands of clips. The
   timeline is drawn with a painter and culled to the visible time range, so
   drawing cost tracks what is on screen rather than what is in the project.
3. **Density.** Professional editing interfaces are dense tool surfaces, which
   is immediate-mode's strength.

The cost is weaker text shaping and accessibility than a native toolkit. The UI
layer is deliberately thin — the entire editor is drivable through
`actions::dispatch` without a window — so replacing it later is contained.

### Everything is an action

Menus, shortcuts and direct manipulation all build an `Action` and hand it to
one dispatch function. The shortcuts and the menu cannot drift apart, the editor
is testable headlessly, and a future macro or plugin layer gets a surface that
already exists.

## What is deliberately absent

Honest gaps, not oversights:

- **No effects yet.** The `Effect`/`ParamValue` model and the render-graph shape
  exist; no effect implementations do.
- **No export.** The renderer can already read frames back, which is the hard
  part; the encoder and muxer are not written.
- **No waveforms, thumbnails, proxies, bins, or ripple editing.**
- **`CpalSink` is unverified.** The audio pipeline is tested up to the sink
  boundary, including the ring under concurrent threads, but the machine this
  was built on has no audio device.
- **Non-linear compositing only**, as described above.

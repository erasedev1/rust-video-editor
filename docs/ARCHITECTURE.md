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

The one thing deliberately outside this system is the **fade** at a clip's end,
which multiplies the animated level rather than being it — see [Fades are not
keyframes](#fades-are-not-keyframes).

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

### Blend modes

A blend mode is a **pipeline variant**, not a shader branch: the pipelines share
one shader module and differ only in their blend state, so the driver compiles
the program once and the whole set is built at startup rather than on the frame a
mode is first used.

Writing `Cs` for the premultiplied source colour, `As` for its alpha and `Cd` for
what is already in the target:

| Mode     | Colour                | Blend state              |
|----------|-----------------------|--------------------------|
| Normal   | `Cs + Cd(1 − As)`     | `One / OneMinusSrcAlpha` |
| Add      | `Cs + Cd`             | `One / One`              |
| Multiply | `Cs·Cd + Cd(1 − As)`  | `Dst / OneMinusSrcAlpha` |
| Screen   | `Cs + Cd(1 − Cs)`     | `One / OneMinusSrc`      |

Alpha composites as the union of coverages in every mode, because a blend mode
describes how colour combines, not how much of the frame the layer covers.

These four are exactly the modes that are a weighted sum of source and
destination, which is all the fixed-function blender can evaluate. Overlay, soft
light, colour dodge, difference and the rest depend on the backdrop in ways no
pair of blend factors expresses; they need it as a *texture*, which means
compositing into an intermediate target and sampling a copy of it, so they belong
with nested compositions rather than here. Darken and lighten look like they would
fit — `min` and `max` are blend operations — but those ignore the blend factors
and so are only correct for a fully opaque layer, and a mode that misbehaves at
50% opacity is worse than a mode that is not there yet.

One deviation is deliberate and tested: `Multiply` does not scale by the
backdrop's alpha, which the Porter-Duff form does, so a multiply layer over a
*transparent* background comes out black instead of showing itself. The default
sequence background is opaque, so this is only reachable when rendering for an
alpha export, and the fully general form needs the same destination read as the
modes above.

The pipeline is bound only when the mode changes from one layer to the next, so a
composition that is all one mode — nearly all of them — still binds once per pass.
Layers keep their back-to-front order rather than being grouped by mode: blending
does not commute, so reordering to save a bind would change the picture.

### The render cache

A composited picture is cached on a **hash of everything the compositor read to
produce it**: target size, background, and per layer the identity of its source
texture and the transform applied to it. A texture is written once at upload and
never again, so the identity of the object stands in for its pixels and a key
costs a couple of hundred nanoseconds rather than a hash of the frame.

The alternative — recording each frame's dependencies and invalidating them when
an edit touches one — has a failure mode that is very hard to test for: a
dependency nobody declared. Add a property to the model, forget to list it, and
the preview shows a stale picture, which looks like the edit did not work.
Content addressing cannot go stale, because a key is derived from the same values
the shader is handed.

**Incremental invalidation then falls out rather than being implemented.**
Changing one clip changes the key of every instant that clip is visible at, so
those instants are recomposited, while every other cached instant keeps its key
and is still a hit. Stale entries are not deleted eagerly; they age out under the
same LRU that bounds the cache.

A repaint takes the cheapest path that is correct:

| Case                                  | Cost                                |
|---------------------------------------|-------------------------------------|
| Composition unchanged since last frame | nothing at all                     |
| Composited before                     | one GPU-to-GPU blit                 |
| Otherwise                             | upload, composite, blit, store      |

The interface draws from one long-lived texture and cached pictures are blitted
into it, rather than each cache entry being registered with egui in turn: a
handle that never changes is cheaper than a handle that changes every frame.
Evicted targets are kept for reuse, because allocating a full-resolution texture
per frame during playback would cost more than the composite being saved.

The unchanged-composition case is the one that matters most today, and it is not
the cache: while editing, most repaints come from the pointer moving over a
panel, and the picture already on screen is still correct. Cache hits come from
revisiting instants — scrubbing back over a cut, replaying a short loop — and
from every extra pass that compositing grows: effects, nested compositions and
colour management all multiply what a hit is worth.

### Colour

Output is **premultiplied**, paired with a `One / OneMinusSrcAlpha` blend. That
is what stops a nested composition double-applying its own alpha at every level.

Frames are **stored** as non-sRGB `Rgba8Unorm`. How they are *interpreted* when
combined is a per-canvas setting, `ColorSpace`:

- **Perceptual** (the default) blends the encoded values directly, as the
  established editors do. A 50% dissolve lands halfway between the two pictures
  as they look, which is what makes a fade feel even end to end.
- **Linear** converts to linear light before blending and back on store. A 50%
  dissolve of white over black comes out at 188 rather than 128, and additive
  highlights stop clipping early.

Neither is wrong, which is exactly why it is a setting rather than a constant.
It defaults to perceptual, and a project written before the setting existed
loads as perceptual, so no existing edit changes underneath anyone.

The conversion is the **hardware's**, not the shader's. Every texture and target
is created able to be viewed as either `Rgba8Unorm` or `Rgba8UnormSrgb`; linear
mode samples and renders through the sRGB views, so the texture unit decodes on
the way in and the output merger encodes on the way out. That keeps the stored
values gamma-encoded, where 8 bits are distributed the way the eye needs them —
blending linear light into a plain `Rgba8Unorm` target would band visibly in the
shadows.

Because the target's format has to match the pipeline's, colour space is an axis
of the pipeline set rather than a uniform: four blend modes times two spaces,
all sharing one shader module.

Two details are easy to get wrong and are pinned by tests. A clear value is
specified in linear light and encoded by the hardware, unlike shader output, so
a background picked as `#808080` has to be *decoded* before it is handed to a
linear pass — encoding it instead washes it out, which is what the first
implementation here did. And the colour space is part of the composite cache
key: the cache is content-addressed, so a space left out of the key would leave
the previous picture on screen until some other input happened to change.

Sequences and compositions carry the setting independently, since a composition
renders to its own target. Pre-composing inherits the sequence's, because
pre-composing is meant to be a reorganisation rather than an edit.

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

### Three threads, one direction

`AudioOutput` owns the whole path. The interface thread owns the device stream
and publishes a snapshot of the project; a mixing thread reads that snapshot,
decodes and mixes ahead, and pushes into the ring; the device callback copies
out. Nothing travels the other way, and nothing the interface does can block the
callback.

The mixer cannot borrow the project the interface is editing, so it gets an
`Arc<Project>` republished whenever the edit changes — one clone of the *edit
model* per edit, no media, which is far cheaper than a lock held across a decode.
A revision counter on the editor state is what makes that once per edit rather
than once per frame.

The picture and the sound are **not** locked to each other. Playback position
for the picture comes from the wall clock and the sound from the device's own
sample counter; the two differ by parts per million, which is nothing over the
length of a cut. Slaving the transport to the audio clock is what a long
programme needs, and it belongs with the rest of the professional audio work
rather than being half-done now.

### Levels, and where they compose

Three gains multiply on the way to the mixer, in this order:

1. the clip's own `volume`, which is a `Property<f64>` and so can be keyframed;
2. the clip's **fades**, one at each end;
3. the **track**'s level, folded in once the clip and anything nested under it
   have had their say.

Pan composes the same way, except that it *adds* and clamps: a track pushed
right carries its clips' own positions with it rather than overriding them.

Track level is a plain number rather than a `Property`, deliberately. A track has
no timeline of its own for keyframes to be relative to, so track automation would
have to be keyed on *sequence* time — a different feature from the clip-relative
animation everything else uses, and one that belongs with keyframe editing rather
than being half-built here.

### Fades are not keyframes

A fade could be two keyframes on `volume`, and that would reuse the animation
system. It is a field of its own anyway, for two reasons that outweigh the reuse:

- A fade **multiplies** the level rather than replacing it. As keyframes it would
  discard whatever level the clip was set to, and every later change to that
  level would have to rewrite the keyframes to keep the fade's shape.
- A fade is anchored to an **end** of the clip, not to a point in time. Trimming
  the tail should carry the fade-out with it; a keyframe at a fixed clip-local
  time would be left stranded in the middle.

Each curve is evaluated from its closed form — `sin`, `cos` — rather than being
approximated by a Bezier ease. That is cheaper *and* exact, and equal-power in
particular only has its defining property if it really is a quarter sine: two
complementary equal-power fades sum to constant **power**, which a test asserts
to within 1e-12 across a thousand points.

Fades that would overlap in a clip shorter than both of them are scaled back
proportionally at evaluation, so a trimmed clip still reaches full level
somewhere; the stored lengths are untouched, so widening the clip again restores
what was asked for.

### A crossfade needs an overlap

A track holds non-overlapping clips by construction, so two clips meeting at a
cut are never audible at the same instant. Fading them into each other there
would mean both reaching silence at the cut — a dip, not a transition. So
`CrossfadeClips` **requires two clips that overlap in time**, which in this model
means two tracks, and refuses anything else with that as the reason. The
alternative would have been to call a pair of fades at a cut a crossfade, which
is the kind of thing that is discovered by ear three hours later.

### Meters read what is being heard

Mixing runs ahead of the device by up to the ring's depth — 200 ms. The block
being mixed is therefore not the block coming out of the speakers, and a meter
fed straight from the mixer would lead the sound by a fifth of a second and never
agree with it.

Each block's reading is instead queued with the sample count it ends at, and the
published reading is the newest one the device has actually reached, worked out
from what is still queued in the ring. Readings are taken **before** limiting, so
a mix that went 6 dB over says so instead of pinning silently at full scale; the
samples themselves are still limited, and the count of limited samples is
reported separately.

Per-track grouping is the caller's business, not the mixer's: `mix_metered`
reports one reading per *source*, and the renderer folds them by track. That is
what lets the same mixer serve a live meter, an export report and a test.

### Waveforms

A minute of stereo 48 kHz audio is 5.8 million sample frames and a few hundred
pixel columns on screen. Reducing the first to the second on every repaint —
during a scroll, a zoom, a drag — would cost more than compositing the picture
does, so the samples are reduced **once**, to a fixed grid of 200 buckets a
second, and every later question is answered from that grid. A bucket keeps the
envelope (min and max) and the RMS level, both reduced across channels: the
envelope is what makes a transient visible, where an average would hide a
single-sample click, and RMS is what the passage actually sounds like.

The grid divides the timebase exactly, so a bucket boundary is a whole number of
ticks and no bucket drifts against the timeline. Twelve bytes a bucket is 2.4 kB
per second of audio, about 8.6 MB an hour, held under a byte budget with LRU
eviction exactly as frames are — peaks for a file nobody is looking at cost a
re-analysis to lose, not a re-edit.

**Analysis is the opposite problem from decoding, so it has the opposite
shape.** A picture is random access: the user is at frame 900 and the answer to
frame 40 is worthless, which is why the decode queue is one slot deep and a new
request cancels the old one. Audio analysis is a linear pass over a whole file,
wanted once, and useful the moment its first second exists. So it is a small
pool of workers over a FIFO, publishing results *as they are produced*. Dropping
a two-hour podcast on the timeline draws a waveform that fills in from the left
while you are already cutting with it. A column past what has been analysed
reports "not known yet" rather than silence, which is what lets the two be drawn
differently instead of a half-read file looking like a half-silent one.

**Coarse levels above the base grid keep drawing bounded at every zoom.** Drawing
zoomed in reads a handful of buckets a column. Drawing an hour-long clip zoomed
all the way out asks a thousand columns to summarise 720,000 buckets, which
measured 3.3 ms — a fifth of a frame, for one clip, on every repaint of a scroll.
Each level summarises eight buckets of the one below, a column reads from the
coarsest level whose buckets still fit inside it, and the same work takes 308 µs.
The levels are summaries of the base grid rather than a second analysis of the
audio, so they cannot disagree with it: a transient in the base grid is in the
envelope of every level above it. See [BENCHMARKS.md](BENCHMARKS.md#waveforms).

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
- **No thumbnails, proxies or bins.** Waveforms exist; the filmstrip on a video
  clip does not, and would be the render cache's problem rather than a new one.
- **Waveforms are a summary, not sample data.** The grid is five milliseconds,
  finer than the eye can use at ordinary zooms and coarser than the editor's
  maximum zoom, where the display interpolates between bucket centres rather
  than storing more. Editing that needs individual samples — repairing a click —
  would read the file, not the peaks.
- **Audio output is unverified on a real device.** The pipeline is tested end to
  end — plan, fades, track levels, mixing, metering and the ring under concurrent
  threads — but the machine this was built on has no sound card, so `CpalSink`
  and `AudioOutput` are the one part no test here has exercised against
  hardware. The editor treats a missing device as an ordinary state and says so
  in the performance overlay rather than failing.
- **No track automation.** Track level and pan are static values; keyframing them
  needs a sequence-time animation domain, which arrives with keyframe editing.
- **No loudness measurement.** The meters are peak meters. LUFS is a different
  measurement with a different purpose and belongs with the delivery work.
- **Non-linear compositing only**, as described above.

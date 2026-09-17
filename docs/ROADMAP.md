# Roadmap

Phases are ordered by what the next phase needs, not by what sounds most
interesting. The rule is that each phase leaves the editor runnable.

## Phase 1 — Foundation ✅

Application shell, project model, media import, sequences, timeline, playback,
save and load. **Complete**; see the README for what that means concretely.

## Phase 2 — Editing ✅

The parts of ordinary cutting that were missing:

- Ripple, roll, slip and slide edits, with a timeline tool for each
- Ripple delete and close gap
- Copy, cut and paste, across tracks
- Multiple selection and marquee selection, and one undo step per operation
  rather than one per clip
- Track add, delete, reorder, lock, mute and solo from the interface
- Marker creation, navigation and deletion
- Clip speed from the inspector

**Complete.** What is not here — fit-to-fill, three- and four-point editing,
insert and overwrite from the source monitor — needs a source monitor first, and
that belongs with the playback work rather than with cutting.

## Phase 3 — Rendering

- Frame-level render caching keyed on the composition, so an unchanged clip is
  not recomposited ✅
- Incremental invalidation: change one clip, recompute only what depends on it ✅
- Blend modes as pipeline variants ✅ — normal, add, multiply and screen
- Nested compositions as layers ✅
- Colour management, with linear-light compositing as an explicit setting ✅

The four blend modes are the ones the fixed-function blender can evaluate from a
premultiplied source, so each costs a pipeline variant and nothing else. Overlay,
soft light and the rest need the backdrop as a *texture* rather than as a blend
factor, which means compositing into an intermediate target and reading a copy of
it — the machinery nested compositions introduce, so they wait for it rather than
arriving as a special case.

Linear-light compositing is a per-canvas setting rather than a constant because
both answers are defensible: blending encoded values makes a dissolve feel even
and matches the established editors, and blending light is how light actually
behaves. Sequences and compositions each carry their own, so a composition can
be authored in linear and laid into a perceptual sequence. It defaults to
perceptual, and a project written before the setting existed loads as perceptual
rather than silently changing every dissolve in it.

The cache is content-addressed — a picture is keyed on a hash of the textures,
transforms, size and background that produced it — so invalidation is a
consequence of the key rather than a list of dependencies that can go out of
date. See `docs/ARCHITECTURE.md` for why, and `docs/BENCHMARKS.md` for what a hit
costs against a composite.

## Phase 4 — Audio

- Waveform generation and display, cached like frames
- Fades and audio transitions
- Track-level volume, pan and meters
- Verified device output across platforms

## Phase 5 — Animation

- Keyframe editing in the timeline
- A graph editor for curves
- Copy, paste and retime keyframes
- Motion blur from the keyframed transform

## Phase 6 — Effects

- Effect registry and dynamic dispatch through the existing `Effect` model
- Blur, colour adjustment, sharpen, transform effects
- Masks and mattes
- Effect ordering and per-clip chains

## Phase 7 — Professional editing

Proxies, multicam, captions, advanced audio, colour grading, hardware encoders,
and export worth the name.

## Phase 8 — Motion graphics

Shapes, text, text animation, nested compositions, motion paths, expressions.

## Phase 9 — Ecosystem

A plugin API, scripting, project interchange (AAF, EDL, OTIO), and documentation
for people extending it rather than building it.

## Standing commitments

These hold across every phase:

- Never block the interface thread on decode, render, I/O or analysis
- Every edit goes through an undoable command
- Add a benchmark before optimising, and quote measured numbers only
- Keep the edit model pure: no I/O, no threads, no GPU in `ve-core`
- Keep the editor runnable at every commit

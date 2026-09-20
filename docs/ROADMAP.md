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

- Waveform generation and display, cached like frames ✅
- Fades and audio transitions ✅
- Track-level volume, pan and meters ✅
- Verified device output across platforms — **built, not verified here**

Waveforms are analysed on background workers and published as they are produced,
so a long file fills in from the left while it is already being cut with, rather
than appearing all at once some minutes later. Peaks live on a fixed grid of 200
buckets a second under a byte budget of their own, evicted least-recently-used
like frames; coarser summaries above that grid keep the cost of drawing
proportional to the columns on screen rather than to the length of the file,
which is what makes an hour-long clip zoomed all the way out cost 308 µs instead
of 3.3 ms. A clip's evaluated volume scales what is drawn, so a fade is visible
in the waveform without any display work of its own — the picture of the sound
and the mixer read the same evaluated gain, so they cannot disagree.

Fades are a field on the clip rather than keyframes on its volume, because a fade
**multiplies** the level rather than replacing it and is anchored to an *end* of
the clip rather than to a point in time. Each curve is evaluated from its closed
form, so equal power really is a quarter sine: two complementary fades sum to
constant power to within 1e-12, which a test asserts rather than assumes. Drag a
grip in a clip's top corner to set one, or use the inspector for an exact length
and shape.

**A crossfade requires two clips that overlap in time.** A track holds
non-overlapping clips by construction, so two clips meeting at a cut are never
audible at the same instant, and fading them into each other there would be a dip
rather than a transition. The command therefore refuses that case and says why,
instead of calling a pair of fades at a cut a crossfade. Put the two clips on
different audio tracks, overlap them by however long the transition should be,
and Ctrl+Shift+F does the rest.

Track level and pan fold into the mix after the clip and anything nested under it
have had their say — gain multiplies, pan adds and clamps. They are plain numbers
rather than animated properties: a track has no timeline of its own for
clip-relative keyframes, so track automation needs a sequence-time domain and
belongs with the keyframe editing in the next phase.

Meters read **what is coming out of the device**, not what is being mixed. The
mixer runs up to 200 ms ahead, so each block's reading is queued with the sample
count it ends at and the published reading is the newest one the device has
actually reached. Readings are taken before limiting, so a mix that went over
says by how much rather than pinning silently at full scale. See
[BENCHMARKS.md](BENCHMARKS.md#audio) for what mixing and metering cost.

**What "not verified here" means.** The whole path is built and tested —
evaluation, fades, track levels, mixing, per-track metering, the ring under
concurrent threads, and the device-facing `AudioOutput` that drives them — but
the container this was developed in has no sound card, so no test here has heard
a single sample. The editor treats that as an ordinary state: it opens without
audio, says so in the performance overlay, and keeps cutting. Ticking this item
needs someone to run it on a machine with a working output on each of the three
platforms; until then it stays as it is written here rather than being marked
done.

## Phase 5 — Animation

- Keyframe editing in the timeline ✅
- A graph editor for curves ✅
- Copy, paste and retime keyframes ✅
- Motion blur from the keyframed transform ✅

**Complete.** The animation editor opens under the timeline with **A** and is
drawn against the timeline's own scroll and zoom, so a keyframe sits directly
under the frame it happens on: there is no second scroll position to keep in
step, because there is no second scroll position.

It shows one of two views of the same selection. The **sheet** answers *when* —
every animatable property of the selected clip as a row, its keyframes as
diamonds, and one button that both keyframes a property and un-keyframes it.
The **curve editor** answers *what* — the value between keyframes, one curve per
channel, with bezier handles on the selected points. Dragging a point there
changes its value and leaves its time alone, and retiming is the sheet's job.
That division is deliberate: a gesture that did both would merge two edits into
the history on every pointer move, and the one the user did not intend is the
one they would notice a minute later.

Curves are drawn by **evaluating the property** at a column of pixels — the same
call the compositor and the mixer make — so a curve cannot draw something other
than what will be rendered. There is no second implementation of easing to
disagree with the first.

Every keyframe edit is one command stating an **absolute** destination: "these
keyframes are now at these times", never "move them three ticks left". Applying
the same edit twice lands in the same place, which is what lets a drag re-issue
itself on every pointer move and still collapse into one undo step. Undo restores
the affected properties whole rather than replaying an inverse, because retiming
two keyframes onto the same tick collapses them and no retime brings the lost one
back.

Two selections coexist — clips and keyframes — so three rules keep them
predictable: taking hold of a clip lets go of its keyframes, Delete and Copy
follow whichever is in hand, and a paste reads whichever clipboard was filled
last. An inspector drag on an animated property now writes a keyframe at the
playhead rather than a static value the animation would go on overriding.

**Motion blur** is two switches, as every compositor has: the shutter belongs to
the canvas, and whether a particular clip is exposed through it is per clip and
off by default, so an older project draws exactly what it drew. The engine
resolves the transform once per sample — and not at all when the layer is not
actually moving during that frame — and the renderer averages the samples into a
target of its own, additively over transparency, which the node above then draws
once. Drawing them straight onto the backdrop at `1/n` opacity each is not an
average: `over` blending makes every sample occlude the ones before it, so a
fully opaque layer would come out about 63% opaque.

What is sampled is the transform, not the source time. Showing frames from
between two frames needs sub-frame decoding or frame interpolation and blurs
footage moving inside itself rather than a layer moving across the frame; that
belongs with the professional work rather than being smuggled in under the same
name. Track automation is the other thing still missing here: a track has no clip
for a keyframe to be local to, so automating one needs a sequence-time domain the
animation system does not have yet.

See [BENCHMARKS.md](BENCHMARKS.md#animation) for what evaluating a property and
averaging a shutter cost.

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

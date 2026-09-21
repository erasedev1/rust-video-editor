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

- Effect registry and dynamic dispatch through the existing `Effect` model ✅
- Blur, colour adjustment, sharpen, transform effects ✅
- Masks and mattes ✅ — shape masks and a luma key
- Effect ordering and per-clip chains ✅

**Complete.** An effect is a string naming a kind and a list of key/value
parameters; the **registry** says what that kind is called and what its
parameters mean, and the **renderer** says which shader it runs. Those three
live apart on purpose. A registry that is a runtime table rather than a match on
an enum is what will let a plugin add an effect in Phase 9 without the core
crate knowing about it — and what already lets a project carrying an effect this
build has never heard of open, render everything else, and save that effect back
untouched rather than dropping it.

Parameters are `Property<T>`, the type every animated value in the editor uses,
so keyframing a blur's radius needs no effect-specific machinery: it is the same
command, the same graph editor and the same evaluation as opacity. That is the
whole reason the animation phase came first.

**Order is the chain.** Blurring and then brightening is not the same picture as
brightening and then blurring, so moving an effect is a real edit with its own
undo step, and the list in the inspector is the pipeline rather than a
presentation of it.

A chain runs **in layer space** — the clip's own picture at its own resolution,
before the clip's transform places it on the canvas — which is what every
compositor does and what keeps a chain stable while the clip moves: a mask
applied after the transform would slide off what it was cut around. Each pass is
cached on its own inputs, so changing one parameter of a five-effect chain
re-runs that pass onwards and leaves the ones before it as hits.

What is **not** here: a track matte, which uses another layer as the matte
rather than a shape or the clip's own brightness. That needs a second input
texture bound to the pass and a rule in the plan for which layer is consumed by
which — a change to the plan's shape rather than one more effect — so it belongs
with the professional work rather than being bolted on. Effect presets and
copying a chain between clips are likewise absent; they are interface work on
top of a model that already supports them.

See [BENCHMARKS.md](BENCHMARKS.md#effects) for what a pass costs, and why a
blur's cost stops growing with its radius.

## Phase 7 — Professional editing

- Export: render a sequence to a file ✅ — H.264, H.265 and ProRes, with sound
- Proxies ✅ — built in the background, switchable, never used for a delivery
- Colour grading ✅ — three-way, white balance, an HSL secondary, and scopes
- Multicam ✅ — grouped cameras, synced on sound or timecode, cut with the
  number keys
- Captions ✅ — written on the timeline, imported and exported as SubRip and
  WebVTT, and written beside a delivery
- Advanced audio
- Hardware encoders

**Export, proxies, grading, multicam and captions are done; advanced audio and
hardware encoders are not.** An editor that cannot produce a file is a
demonstration rather than a tool, so export came first.

What an export is, is the editor run with nobody watching. The same `evaluate`
the preview calls resolves each instant; the same compositor draws it — one
piece of code, shared, because an export that composited by a second route would
eventually disagree with what the editor showed, and the disagreement would be
discovered in the delivered file. The same mixer produces the sound. Only the
rules about *time* differ, and only in the two ways that matter:

| | Preview | Export |
|---|---|---|
| A frame that is not decoded yet | is dropped | is waited for |
| The clock | drives the picture | is the frame index |

Playback is a real-time system with a deadline it must not miss. An export is a
batch job with an answer it must not get wrong.

Three decisions are worth stating. **Sound is counted in samples, not in
frames**: how much audio belongs to one video frame is not a constant at 29.97,
so each block is the difference between two absolute sample indices — over an
hour that is the difference between sync and a frame and a half of drift.
**Compositing happens at the sequence's own resolution** and the picture is
scaled on the way into the encoder, so a half-size review copy is the picture
the editor showed rather than a different composite with smaller masks and
softer blurs. And **a cancelled export deletes what it had written**, because a
part-written MP4 has no index and would sit there looking like a deliverable.

The exporter runs on its own thread and reports progress; the editor keeps
running — keeps playing, keeps being edited — while it works, from a snapshot of
the project taken when the button was pressed.

Colour is now said out loud rather than assumed. swscale's default matrix is
BT.601 whatever the picture's size, which is wrong for anything HD by more than
twenty 8-bit levels on saturated colour. The exporter converts with the matrix
it tags the file with — 709 from 720 lines up, 601 below — and the *decoder*
reads a file with the matrix that file declares, falling back on its size the way
a player does. So importing Verge's own export gets back what it put in, which a
test asserts frame by frame.

What is **not** here. There is no alpha export: every codec above subsamples
chroma and drops the alpha channel, and keeping it needs ProRes 4444 and a
compositing path that does not assume an opaque background. There are no
hardware encoders — NVENC, Quick Sync, VideoToolbox are each a different device
to feed and none of them can be tested in the container this was developed in,
so claiming them would be claiming something unverified. And there is no
image-sequence or audio-only output, both of which are a container away rather
than a feature.

See [BENCHMARKS.md](BENCHMARKS.md#export) for what a written frame costs.

### Proxies

A proxy is a smaller stand-in for one file's **picture**. It carries no sound —
the mixer and the waveforms read the original whether or not one exists — so
building one never re-encodes audio and there are never two sound tracks that
could drift apart. There is no code keeping them in step because there is
nothing to keep in step.

What a proxy buys is not disk space; it adds to that. It buys latency, and in
two parts that are easy to run together. A quarter on each axis is a sixteenth
of the pixels. But a proxy is also written **all-intra**, so a jump into the
middle of the file costs that frame rather than every frame back to the last
keyframe — and on this container that second part is worth about two and a half
times again on top of the resolution. Playing forward gets 28× cheaper;
scrubbing gets 72×. The gap between those two numbers *is* the intra-frame
decision, which is why editing formats have always been intra-frame and why a
proxy that was merely a downscaled re-encode would leave most of the benefit
unclaimed. See [BENCHMARKS.md](BENCHMARKS.md#proxies).

Four rules are worth stating.

**An export always renders the originals.** A proxy exists so the editor can
keep up with a hand on a mouse; a delivery has nowhere to be and every reason to
be right. A delivery rendered from the stand-in would be a soft file nobody
asked for, and whoever was handed it would be the one to discover that. So the
exporter reads the original path directly and does not consult the setting at
all — asserted by a test that fails if it is ever changed to.

**Using them is a switch, not a consequence of one existing.** The whole point
of building a proxy is to be able to turn it off and look at the real picture —
checking focus, checking a key — without throwing it away. The switch lives on
the project, so reopening a cut resumes at the resolution it was being cut at.

**A missing proxy is not a missing asset.** A proxy is a convenience the editor
built for itself, so one that has been deleted — a cleared folder, a project
moved without it — falls back to the original and carries on at full
resolution. Going offline over it would lose the user's footage because the
editor lost its own scratch file. The reference is kept rather than dropped, so
restoring or rebuilding the file simply works again.

**Source timestamps are preserved rather than recounted.** A proxy that showed
a different frame from its original at the same instant would have the cut made
against one picture and delivered from another. For constant-rate footage the
frame index and the timestamp are the same number; for variable-rate footage
they are not, and recounting would silently re-time the file. A test walks all
ninety frames of the fixture through both files and asserts they agree, at 30
and at 29.97.

Building runs on its own thread, one file at a time, and the editor keeps
playing and keeps being edited while it works. One unreadable clip does not stop
the batch. A build refuses to start until the project has been saved, because
proxies live beside the project file and writing them anywhere else would leave
a folder of files nothing ever points at again.

What is **not** here. There is no automatic proxy on import: transcoding a card
the moment it is dragged in is a decision to spend an hour of someone's machine,
and it should be asked for. There is no per-clip override of the switch, for the
reason given above — a sequence shown half at one resolution and half at another
is lying about what it looks like. And nothing rebuilds a proxy when the footage
behind it changes on disk: "Rebuild All Proxies" is the answer, and a file
watcher that noticed by itself is a different piece of machinery.

### Colour grading

Three effects and four instruments, and the effects are the smaller half of
that sentence.

**The three effects needed no new machinery.** A three-way corrector, a white
balance and an HSL secondary are registry entries and shader entry points; the
inspector built their controls, the animation system keyframed their
parameters, the render cache keyed their passes and the project format saved
them, all without changing. Phase 6 claimed that the registry would let an
effect arrive with working controls and keyframes for nothing. This is the
first effect added *since* that claim, and it is the evidence for it.

A three-way corrector is lift, gamma and gain per channel: `out = (in · slope +
lift) ^ exponent`, where an input of 0 comes out at `lift` and an input of 1 at
`gain`. The six controls the user sees resolve into those three vectors on the
CPU, so the shader is a multiply, an add and a `pow`, and a full grade costs
what a touched one does. The neutral of a wheel is the *middle* of its range,
because a wheel is an offset rather than a colour to push towards; every
control at neutral gives exactly the identity, which a test asserts rather than
assumes.

A white balance is drawn by the **colour adjust** program. A temperature and a
tint are a per-channel multiply, which that shader already does, so what the
kind adds is the arithmetic between two intuitive controls and three gains —
divided by their own Rec. 709 luma, so a cooling is not also a darkening. Two
kinds, one program: the list the edit model names and the list the GPU runs are
deliberately not the same list, which is what will let a plugin reuse a
built-in program later.

The secondary selects a band of hue above a saturation floor and grades only
that. Hue is carried in turns so the wrap is `fract` — red sits at both ends of
the circle and a band centred on it has to reach across the seam — and the
floor is what keeps greys, whose hue is whatever the arithmetic happened to
produce, out of the key. `Show Matte` draws the selection in black and white,
because dialling a qualifier by looking at the graded picture means guessing at
the edges of the key from the other side of a grade.

**The scopes are the half that needed building.** A waveform in luma, RGB
overlay or parade; a vectorscope; a histogram. They read the composited
frame — after the grade, after every effect, after the blend — because what is
being judged is the picture that will be delivered rather than an estimate
assembled from the parameters that made it.

They read it small: scaled into a target 256 across and read back from there. A
scope is a statistic, and thirty thousand samples locate a black level far
better than the width of a line on screen; reading an HD target back instead
costs eight megabytes over the bus and a stall to wait for it, every frame. So
the cost of a scope is bounded by the scope rather than by the resolution of
the sequence — a 4K timeline counts the same 36,864 pixels. The consequence is
said out loud rather than hidden: a scope here will not show a single stray hot
pixel. It is an instrument for judging a grade, not for auditing a delivery.

Nothing is read back while the panel is closed, which is the default, and a
picture already sampled is not sampled again — which is the common case,
because a grade is dialled in on a held frame and the same composite is
presented on every repaint.

What is **not** here. There is no **curve** — the tone curve is the other half
of a grading toolkit, and it needs a parameter kind that is not a number, a
point or a colour, and a control that is not a slider. That is a change to the
registry's shape rather than one more entry in it, which is why it waits rather
than being bolted on. There is no **LUT**, which needs a 3D texture and a file
reference in the edit model, and brings the question of what happens when the
file moves — the same question proxies answer and that a LUT would have to
answer differently. There are no **tracked** secondaries, and no shot matching:
both are analysis over time rather than a shader, and belong with the
professional work the rest of this phase is. And the scopes have no **graticule
of primary targets**, for the reason given in
[ARCHITECTURE.md](ARCHITECTURE.md#scopes): those boxes are 75% bars under one
standard, and drawing them over a Rec. 709 plot would invite a reading they do
not support.

See [BENCHMARKS.md](BENCHMARKS.md#grading) for what a grading pass costs
against a plain composite, and [the scopes table](BENCHMARKS.md#scopes) for
what reading the picture back and counting it costs — including the `f64::round`
that turned out to be most of the second one.

### Multicam

A group is a set of angles on one shared timeline, each carrying the offset from
that timeline into its own media. A clip draws `Source::Multicam{group, angle}`,
so **cutting between cameras changes one field and nothing else** — the clip
keeps its position, its length, its window into the group, its effects and its
animation, because none of those is about which camera is being watched. That is
what makes a cut instant, and what makes it undoable as one tiny command rather
than as a rewrite of the clip.

The alternative — swapping the clip's asset and rewriting `source_in` by the
difference of two offsets — would make every angle change a different edit
depending on which angle it came from, and a group synced wrongly would leave a
trail of clips already rewritten with the wrong numbers.

An angle is a **camera**: an `AssetId`, not a `Source`. A composition cannot be
an angle and neither can another group, which keeps the nesting graph exactly as
it was so the cycle check that stops a render never finishing does not have to
learn about groups. An angle that needs work done to it is a clip with effects
on it, or a multicam clip inside a composition — the direction nesting already
runs.

A clip is trimmed against the **group**, not against the angle it happens to be
showing, or the same clip would have different limits depending on what was on
screen when its edge was grabbed. The group covers the **union** of its cameras
rather than the intersection: cameras start and stop at different moments, and
bounding to the stretch every camera covers would refuse edits over footage that
plainly exists. An angle with nothing at a given instant draws nothing, and the
viewer says which those are.

**Syncing correlates the envelope, not the samples.** Two cameras twenty feet
apart record the same event through different microphones, at different levels,
with different room colouration; their waveforms do not match at all, and
correlating 48,000 samples a second to discover that is expensive as well as
wrong. What matches is *when the loud parts happen* — so this correlates the RMS
series the waveform analysis already produces, at 200 buckets a second. Scores
are Pearson over the overlap, so a camera set 14 dB quieter matches exactly.

Five milliseconds is not frame-accurate at 24 fps and the code does not pretend
otherwise: every match carries a **confidence**, the interface reports the
weakest pair in the group, and the offsets can be nudged by hand afterwards —
which marks the group Manual, because once a number has been typed over, saying
it was measured is no longer true. Timecode syncing reads the start timecode the
container records, drop frame included, and refuses rather than guessing when a
camera does not carry one.

The viewer is a grid of every camera at one instant, with the one on screen
outlined. Numbers cut, shift switches, and a tile is numbered over *every* angle
rather than only the enabled ones — disabling a camera must not renumber the
keys under the user's fingers part way through a shot. A camera that was not
rolling keeps its tile and says so, for the same reason.

Tiles are uploaded as ordinary egui textures rather than going through the
compositor: a tile is a thumbnail of one decoded frame with no transform, no
effects and no blending, so it needs no render target and no pipeline. The
frames come from the cache and the scheduler the preview already uses.

See [BENCHMARKS.md](BENCHMARKS.md#multicam-syncing) for what syncing costs and
what the coarse-to-fine search is worth against the exhaustive one.

**A bug this found.** Building a 16 kHz fixture for the sync tests exposed a
silent fault in the audio decoder: `ffmpeg-next` allocates the resampler's
output frame for the input's sample count, so upsampling never caught up. A
44.1 kHz file played 8.8% fast and drew its waveform 8.8% short, believed by
everything downstream. Every committed fixture was 48 kHz, where the bug cannot
show. Fixed, with regression tests in both directions.

### Captions

A cue is text with a span, and that is all it is. Written as a clip it would
arrive carrying a source window, a speed, a transform, a blend mode, an effect
chain and a pair of audio properties, every one of which would need a rule
saying it means nothing here — so captions are their own small type on the
sequence, and the code that walks clips is not made to understand a clip that is
not one.

What is kept from the clip model is the **invariant**: cues are sorted and never
overlap, so what is on screen at an instant is a binary search with exactly one
answer. Two speakers at once is one cue of two lines, which is what every
captioning standard asks for and what a reader can actually follow. A second
caption *track* is for a second language, and carries a BCP 47 tag because that
tag names the file the track exports to.

**Captions are the one part of an edit people routinely author elsewhere** — a
transcription service, a captioner working to a broadcaster's style guide, a
colleague with a text editor — so SubRip and WebVTT are not a convenience beside
the feature. They are how captions get into an edit and how they leave it. One
reader accepts both formats and the writer emits exactly one: the two differ in
a header line, a separator and three block keywords, and files in the wild
ignore even those differences. Liberal in, exact out, so a round trip through
another tool does not degrade a file a little more each time. A file that is not
valid UTF-8 is read as Latin-1 rather than refused, and a malformed cue costs
its own block rather than the file.

Both formats count milliseconds, which divide the tick base exactly, so reading
is lossless and writing rounds by at most half a millisecond — a sixtieth of a
frame at 29.97. Cues are deliberately **not** snapped to the frame grid: a
caption is text rather than a picture, and snapping would shift every imported
cue and make the round trip lossy.

Four decisions are worth stating.

**The preview draws an overlay, not a burn-in.** The interface draws the current
cue over the picture with its own text; the compositor never sees it. Burning
captions into the frame needs a glyph rasteriser in the render graph, which is
the next phase's work, and claiming it here would mean a preview showing
something the delivery would not have.

**Whether captions are drawn is a view setting, never project data.** A track
that could be hidden from the preview *and* from an export would be a trap: hide
it to check a shot, deliver without it, hear about it from the client. So the
editor has a switch and an export writes every track it is asked for.

**An export writes them beside the file**, timed from the start of the exported
range — a ten-minute delivery from the middle of a cut is its own file starting
at zero — and after the trailer, so a cancelled export cannot leave caption
files behind for a delivery that no longer exists. A cue straddling the in point
is clipped rather than dropped, because half that line is spoken inside the
range.

**An import lands on a track of its own** rather than replacing the selected
one. An import is usually a language arriving, and quietly overwriting an hour
of someone's corrections because the wrong lane was selected is not a mistake
worth making possible.

What is **not** here. There is no burn-in and no subtitle stream muxed into the
container, for the reasons above. Captions carry no position, alignment or
styling: a cue is text and a span, and a file that has those says so on import
rather than being half-honoured. There is no speech recognition — that is a
model, not an editor feature, and the file it would produce is one this already
imports. And a ripple does not carry cues with it, because a ripple here is
per-track and a caption lane is not the track being rippled.

See [BENCHMARKS.md](BENCHMARKS.md#captions) for what reading a film's worth of
captions costs, and what finding the one on screen costs per frame.

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

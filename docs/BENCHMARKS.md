# Benchmarks

Every number here was measured with `cargo bench`. Nothing is estimated.

Run them yourself:

```sh
cargo bench                                   # everything
cargo bench -p ve-core --bench timeline       # edit model
cargo bench -p ve-project --bench project_io  # save and load
cargo bench -p ve-render --bench compositing  # GPU, including effect passes
cargo bench -p ve-media  --bench waveforms    # audio analysis and display
cargo bench -p ve-engine --bench audio        # mixing, metering and fades
cargo bench -p ve-export --bench export       # encoding, readback and a whole second
cargo bench -p ve-export --bench proxy        # what a proxy costs and what it buys
```

## The machine these were taken on

A 4-core container with **no GPU**: rendering ran on Mesa's `llvmpipe` software
rasteriser. CPU figures are meaningful; GPU figures are not representative of
real hardware and are included only so that a regression in the amount of work
per frame shows up as a change.

Criterion reports `[lower estimate upper]`; the middle value is quoted below.

## Edit model

| Benchmark                       |    100 clips |  1,000 clips | 10,000 clips |
|---------------------------------|-------------:|-------------:|-------------:|
| `build_timeline`                |    11.6 µs   |     127 µs   |     2.30 ms  |
| `clips_in_visible_range`        |    37.7 ns   |    50.9 ns   |    90.8 ns   |
| `clip_at_playhead`              |     8.7 ns   |            — |    21.6 ns   |
| `snap_candidate`                |    63.3 ns   |            — |     127 ns   |

`clips_in_visible_range` is the number that matters most. It grows by a factor
of **2.4 while the project grows by 100**, which is the whole point of holding
the sorted, non-overlapping invariant: timeline drawing costs what is on screen,
not what is in the project.

### A bug this suite caught

`build_timeline` originally measured **10.6 µs / 212 µs / 10.7 ms** — plainly
quadratic. `Track::is_range_free`, called by every insert and every move, was
scanning the entire track instead of binary-searching to the only region that
could overlap. Fixing it took the 10,000-clip case from 10.7 ms to 2.3 ms, a
**4.6× improvement**, and flattened the curve.

The residual super-linearity is `Vec::insert` shifting elements, which is
inherent to a contiguous sorted list and not yet worth changing.

### Other edit operations

| Benchmark                | Time     |
|--------------------------|---------:|
| `move_clip_1000`         |  64.0 ns |
| `split_and_restore_1000` |  36.3 µs |

`split_and_restore` clones a 1,000-clip project per iteration; the split itself
is a small fraction of that figure.

### Time conversion

| Benchmark                       | Time     |
|---------------------------------|---------:|
| `frame_to_ticks_ntsc`           |  2.64 ns |
| `ticks_to_frame_ntsc`           |  4.01 ns |
| `timecode_from_frame_dropframe` |  15.8 ns |

Exact rational time at 29.97 fps costs a few nanoseconds. There is no
performance argument for floating-point seconds.

### Animation

Evaluating a property is what every animated value costs, once per frame — and
with motion blur on, once per **sample** per frame.

| Benchmark                          |     Time |
|------------------------------------|---------:|
| `evaluate_property/constant`       |  0.99 ns |
| `evaluate_property/linear/2`       |  1.26 ns |
| `evaluate_property/linear/64`      |  11.1 ns |
| `evaluate_property/linear/1000`    |  18.8 ns |
| `evaluate_property/bezier`         |  68.2 ns |
| `evaluate_transform/once`          |  16.7 ns |
| `evaluate_transform/twelve_samples`|   323 ns |

Three things worth reading off this:

- **A constant costs a nanosecond**, which matters because nearly every property
  in a project is one: the evaluation returns before touching the keyframe
  vector, so animation is not a tax on the properties that do not use it.
- **A thousand keyframes cost 15× a single segment, not 500×.** Evaluation
  binary-searches, so a heavily animated property is still a lookup rather than a
  walk.
- **A hand-shaped bezier costs about four times a preset.** The named presets are
  all either linear or solved by the same Newton iteration; the 68 ns is the
  solver converging, and it is the reason the linear case short-circuits before
  reaching it.

`twelve_samples` is what a motion-blurred item costs the engine before the GPU
sees anything: twelve transform evaluations, one per instant the shutter is open.
At 323 ns it is under a thousandth of the frame it belongs to — the cost of blur
is entirely on the GPU side, which is where the next table is.

## Project save and load

| Benchmark        |   100 clips | 1,000 clips | 10,000 clips |
|------------------|------------:|------------:|-------------:|
| `save_project`   |     1.02 ms |     4.57 ms |      36.1 ms |
| `load_project`   |    0.41 ms  |     5.53 ms |       105 ms |
| `serialise_to_json` |        — |     1.84 ms |            — |

Both scale roughly with document size, as JSON work should.

### A second bug this suite caught

`load_project` originally measured **1.04 ms / 11.3 ms / 197 ms**. The reader
was parsing the file into a `serde_json::Value`, serialising that back to a
string, and parsing it a second time — an artefact of wanting a single parsing
path. Reading and parsing once **halved** load time at every size.

## GPU compositing

Software rasteriser. Treat these as relative, not absolute.

| Benchmark             | Time     |
|-----------------------|---------:|
| `upload_frame/360p`   |  3.45 ms |
| `upload_frame/1080p`  |  10.0 ms |
| `composite_1080p/1`   |  5.51 ms |
| `composite_1080p/4`   |  17.3 ms |
| `composite_1080p/16`  |  77.6 ms |
| `layer_matrix`        |  10.6 ns |

Compositing scales linearly with layer count, which is what a rasteriser filling
1080p once per layer should do. On real hardware these are fill-rate bound and
far cheaper.

### Blend modes

Sixteen 1080p layers, measured in one run:

| Benchmark                              |     Time |
|----------------------------------------|---------:|
| `composite_1080p_16layers/one_mode`    |  102 ms  |
| `composite_1080p_16layers/alternating` |  105 ms  |

Fifteen extra pipeline binds cost **2.6%** here, which is why the renderer binds
only when the mode changes rather than once per layer — and also why it is not
worth reordering layers to group them by mode, which would change the picture for
a saving this size.

### Motion blur

Taken in one run on the same container as the animation table above, so the
plain composite was re-measured alongside it rather than compared across runs.
One 1080p layer, averaged across N samples:

| Benchmark                | Time     | Against one draw |
|--------------------------|---------:|-----------------:|
| `composite_1080p/1`      |  5.12 ms |               1× |
| `motion_blur_1080p/1`    |  4.90 ms |            0.96× |
| `motion_blur_1080p/4`    |  15.4 ms |             3.0× |
| `motion_blur_1080p/12`   |  41.5 ms |             8.1× |
| `motion_blur_1080p/32`   |   104 ms |            20.3× |

The shape is what to look at. Blur costs **a draw per sample and nothing else**:
one sample is the same price as compositing one layer, and twelve samples cost
about what twelve layers would. There is no per-sample upload, allocation or
pipeline bind — the samples share one texture, one uniform buffer and one pass —
which is why twelve samples come in under twelve times one rather than over it.

That is also the argument for the default of twelve rather than thirty-two, and
for the engine refusing to sample a layer that is not moving during this
particular frame: the cheapest blurred frame is the one that was never blurred.
On real hardware these are fill-rate bound and far cheaper, but the ratio
carries.

### The render cache

Taken in a single run on a later container — also llvmpipe, but a different
machine from the table above, so the plain composite was re-measured alongside
the cache rather than compared across runs. Four 1080p layers throughout,
`--sample-size 20`:

| Benchmark                              |     Time | Cheaper by |
|----------------------------------------|---------:|-----------:|
| `composite_1080p/4` (no cache)         | 29.3 ms  |         1× |
| `render_cache_1080p_4layers/miss`      | 28.7 ms  |      1.02× |
| `render_cache_1080p_4layers/hit`       | 0.74 ms  |        40× |
| `render_cache_1080p_4layers/key`       |  254 ns  |   115,000× |

Three things worth reading off this:

- **A hit is 40× cheaper than the composite it replaces** here, because it is one
  GPU-to-GPU blit instead of four full-frame draws. On real hardware both sides
  shrink; the ratio is what carries over, and it grows with every pass compositing
  gains.
- **A miss costs no more than compositing without a cache at all.** The extra blit
  and the store are inside the noise of the composite itself, so the cache is not
  a tax on the frames it fails to serve.
- **Keying is free.** 254 ns against a 29 ms composite is 0.0009%, which is what
  makes it reasonable to compute a key on every repaint — including the repaints
  that then do nothing because the composition has not changed.

The cheapest case is not in the table: when the composition is unchanged the
preview does no GPU work at all, so there is nothing to measure but the key.

### Colour space

Perceptual against linear light, four 1080p layers, same scene:

| Colour space | Time     |
|--------------|---------:|
| Perceptual   | 16.80 ms |
| Linear light | 25.00 ms |

**Linear costs about 48% more here, and that number is about the rasteriser
rather than about the design.** Linear compositing adds no shader work, no extra
pass and no copy: the conversion belongs to the texture unit on sample and the
output merger on store. Both are fixed-function on a GPU and effectively free.
llvmpipe has no such hardware, so it executes the sRGB transfer function per
texel and per pixel in software, and that is what this measures.

What the figure is good for is catching a regression that moves the conversion
somewhere it does not belong — into the shader, or into an extra pass. What it
should not be read as is the cost on real hardware, which is not measured here
because this machine has no GPU.

### Effects

Taken in one run on a later container — llvmpipe again, and a different machine
from the tables above, so the plain composites were re-measured alongside the
effects rather than compared across runs. One 1080p source, `--sample-size 10`:

| Benchmark                           |     Time | Against one composite |
|-------------------------------------|---------:|----------------------:|
| `composite_1080p/1`                 |  5.31 ms |                    1× |
| `composite_1080p/4`                 |  20.5 ms |                  3.9× |
| `effect_pass_1080p/transform`       |  4.71 ms |                 0.89× |
| `effect_pass_1080p/mask`            |  4.91 ms |                 0.92× |
| `effect_pass_1080p/luma_key`        |  5.51 ms |                  1.0× |
| `effect_pass_1080p/color`           |  7.62 ms |                  1.4× |
| `effect_pass_1080p/sharpen`         |  15.1 ms |                  2.8× |
| `effect_pass_1080p/blur_one_axis`   |   183 ms |                   35× |
| `chain_passes_3_effects`            |  89.4 ns |                       |

What to read off this:

- **A pass costs about what a layer costs**, plus its shader. Transform, mask
  and luma key are one texture read per pixel and land within 10% of a plain
  composite, which is the right sanity check that the pass machinery itself —
  the target, the uniform, the bind, the submit — is not where the time goes.
- **The shader is the variable, and the blur is the outlier.** Sixty-five taps
  per pixel is sixty-five texture reads, and a software rasteriser charges full
  price for every one; on a GPU these are the cache-friendliest reads there are.
  Sharpen's nine taps landing at 2.8× a one-tap pass says the same thing from
  the other end.
- **Describing a chain is free.** Turning three resolved effects into their four
  passes costs 89 ns against passes measured in milliseconds, which is what
  makes it reasonable to rebuild the pass list every frame rather than caching
  it and having to work out when it went stale.

#### The blur's tap cap

One axis, at four radii, re-measured on its own with `--measurement-time 8`:

| Radius |     Time |
|--------|---------:|
| 1      |  41.1 ms |
| 8      |   182 ms |
| 64     |   180 ms |
| 200    |   182 ms |

**Past a radius of about 5, a blur stops getting more expensive.** The kernel
is sampled a bounded number of times, so a wider blur spreads its taps out
rather than taking more of them: 8, 64 and 200 are all 65 texture reads, and
only the distance between them changes. A radius of 1 is cheaper still because
its support is narrower than the budget, so it takes 13 taps rather than 65.

What is traded away is exactness at the top of the range: at 200 the taps are
about 19 texels apart, which is a coarse approximation of a Gaussian that wide.
That is the trade every real-time blur makes, and it is why the parameter is
called a radius rather than a promise — but it is a trade in *quality*, not in
cost, which is what this table is here to show.

## Waveforms

Taken on the same container as the render-cache table, `--measurement-time 3`.

### Analysis

| Benchmark                  |      Time | Against real time |
|----------------------------|----------:|------------------:|
| `waveform_reduce/1ch_1s`   |   192 µs  |           5,200×  |
| `waveform_reduce/2ch_1s`   |   382 µs  |           2,600×  |
| `waveform_reduce/6ch_1s`   |  1.16 ms  |             860×  |
| `waveform_analyse_1s_wav`  |  10.0 ms  |             100×  |

`waveform_reduce` is samples to buckets and nothing else; `waveform_analyse` is
the whole path including opening the file and decoding it. The gap between them
is the answer to whether the reduction is worth optimising: it is **3.8% of the
end-to-end cost**, and the other 96% is FFmpeg. So an hour of audio analyses in
about half a minute, and making the bucketing twice as fast would take a second
off that.

### Display

This is the per-repaint cost, for one clip, and it is the figure the design is
built around: reducing peaks to pixel columns has to cost what is on screen
rather than what is in the file.

| Benchmark                                     |     Time |
|-----------------------------------------------|---------:|
| `waveform_envelope/clip_600px_of_1min`        |  22.7 µs |
| `waveform_envelope/clip_600px_of_60min`       |  22.3 µs |
| `waveform_envelope/zoomed_past_the_grid_600px`|  13.8 µs |
| `waveform_envelope/whole_hour_1000px`         |   308 µs |

**The first two are the claim.** Six hundred columns cost the same whether they
are drawn from a one-minute file or a sixty-minute one — 2.4% apart, which is
noise. A clip on screen costs what it takes up, not what it references.

### The pyramid this bought

`whole_hour_1000px` is the hard case: an hour-long clip zoomed all the way out,
a thousand columns summarising 720,000 buckets. Reading every bucket measured
**3.30 ms**, a fifth of a frame budget for one clip, on every repaint of a
scroll. Keeping coarser copies of the peaks — each summarising eight buckets of
the one below — brought it to **308 µs**:

| Zoomed all the way out, 1,000 columns |     Time | Cheaper by |
|---------------------------------------|---------:|-----------:|
| Base grid only                         |  3.30 ms |         1× |
| With the pyramid                       |   308 µs |      10.7× |

The coarse levels add **under 15% to memory** and are summaries of the base grid
rather than a second pass over the audio, so they cannot disagree with it: a
transient in the base grid is in the envelope of every level above it. A test
asserts exactly that, across four zoom levels.

## Audio

Same container, `cargo bench -p ve-engine --bench audio`. Nothing here touches
the GPU, so unlike the compositing figures these are representative.

### Mixing

Each block is 4,800 stereo sample frames — a tenth of a second at 48 kHz — with
every source at a different pan position. The figure that matters is the last
column: a device will not wait, so the headroom a session has is the multiple of
real time the mixer achieves.

| Sources |     Time | Against real time |
|---------|---------:|------------------:|
| 1       |  62.1 µs |           1,611×  |
| 4       |   115 µs |             872×  |
| 16      |   326 µs |             307×  |
| 64      |  1.19 ms |              84×  |

Sixty-four simultaneous audio clips still mix **84× faster than they play**, on
four cores with no vectorisation beyond what the compiler found. The ring holds
200 ms, so that is a very large margin against a slow decode.

### Metering costs nothing measurable

The same blocks, mixed with per-source peak metering on:

| Sources | Unmetered |  Metered | Difference |
|---------|----------:|---------:|-----------:|
| 1       |   62.1 µs |  62.7 µs |     +0.9%  |
| 4       |    115 µs |   114 µs |     −0.4%  |
| 16      |    326 µs |   327 µs |     +0.6%  |
| 64      |   1.19 ms |  1.18 ms |     −0.8%  |

Two of the four differences are **negative**, which is the honest way of saying
the effect is inside the noise: the peak is taken from values the mix loop has
already computed and holds in registers. A meter that cost anything real would
be a meter people turned off.

### Fades are cheap, but not free

This one did not come out the way the claim was written, so the claim was
changed rather than the number.

| Benchmark                | No fade |  Faded | Difference |
|--------------------------|--------:|-------:|-----------:|
| `audio_envelope`         | 12.2 ns | 22.6 ns |    +86%   |
| `audio_plan/4 tracks`    |  181 ns |  240 ns |    +33%   |
| `audio_plan/16 tracks`   |  723 ns | 995 ns  |    +38%   |
| `audio_plan/64 tracks`   | 2.44 µs | 3.34 µs |    +37%   |

A fade **roughly doubles** the cost of evaluating one clip's audio envelope —
two extra `sin`/`cos` evaluations against what was a pair of branch-predictable
lookups — and adds about a third to resolving the whole instant.

It does not matter, and here is the arithmetic that says so. The plan is
evaluated **once per mixed block**, not once per sample, and the mixing thread's
block is 50 ms of audio. Sixty-four faded tracks cost 3.34 µs against that:
**0.007% of the time available**. Against the 1.19 ms the same 64 sources take
to mix, it is 0.3%.

The reason to record it anyway is that the shape would change if fades ever moved
onto the per-sample path — a per-sample envelope at 48 kHz would be 22.6 ns ×
48,000 = 1.1 ms a second per clip, which is no longer nothing. They are not there,
and this table is why they should not go there.

## Export

`cargo bench -p ve-export --bench export`, on the same container. The encoder is
libx264 at its `medium` preset; the readback figures are a software rasteriser's
and are here for the shape rather than the value.

### One frame, in the parts this suite adds

| Stage                       |     360p |    1080p |
|-----------------------------|---------:|---------:|
| Encode (swscale + libx264)  |  1.26 ms | 10.73 ms |
| Read back from the GPU      |   223 µs |  1.68 ms |

Those are the two stages an export has that a preview does not. The third —
compositing — is the same call the preview makes and is measured in the [GPU
table](#gpu-compositing) above, where one 1080p layer costs 5.51 ms on this
container's software rasteriser.

The encode is measured against deliberately noisy pictures, which is the worst
case: a picture with detail in every block is what an encoder spends its time
on, and real footage is easier.

### A whole second, end to end

Thirty frames of a real timeline — decode, composite, read back, encode, mux:

| Canvas |    Time | Per frame | Against real time |
|--------|--------:|----------:|------------------:|
| 360p   |  154 ms |   5.14 ms |             6.5×  |
| 1080p  |  818 ms |   27.3 ms |             1.2×  |

So an export of a 1080p sequence on **four cores with no GPU** runs at about
real time: a ten-minute programme takes a little over ten minutes. That is the
honest figure for the worst hardware this is likely to meet.

What the split says about where to spend effort next. At 1080p the encode is
10.7 ms of that 27.3 — **39% of the frame** — and the two stages beside it,
compositing and readback, are the two that a real GPU makes small. On any
machine with hardware worth the name the encoder is therefore not one cost
among several but the cost, and it is the one stage that is not ours: libx264
is already the fastest thing in its class at this quality. That is the argument
for the hardware encoders listed in Phase 7 — not that software encoding is
slow, but that everything else has somewhere to go and it does not.

## Proxies

`cargo bench -p ve-export --bench proxy`. The committed fixtures are 160×120,
which is below the size at which a proxy is worth having, so this suite writes
its own 1080p source first: ninety frames of noise at 30 fps, encoded long-GOP
with a keyframe a second, which is what a camera produces. The proxy is a
quarter on each axis — 480×270, a **sixteenth of the pixels** — and all-intra.

### Scrubbing

Eight jumps to scattered frames, forwards and backwards, through one open
decoder. This is what dragging a playhead does, over and over.

| File                     | 8 jumps  | Per jump  |
|--------------------------|---------:|----------:|
| Original, 1080p long-GOP |  2.51 s  |   314 ms  |
| Proxy, 270p all-intra    | 34.7 ms  |  4.34 ms  |

**72× faster.** Three hundred milliseconds is a playhead that lags visibly
behind the mouse; four is one that does not.

### Playing forward

Thirty frames in order, no seeking:

| File                     | 30 frames | Per frame |
|--------------------------|----------:|----------:|
| Original, 1080p long-GOP |    746 ms |   24.9 ms |
| Proxy, 270p all-intra    |   26.7 ms |  0.89 ms  |

**28× faster**, against a sixteenth of the pixels. More than the pixel count
alone because a proxy is also a smaller bitstream to read and a smaller
conversion to RGBA on the way out.

### What the two numbers say together

This is the part worth reading twice. Playing forward improves **28×**;
scrubbing improves **72×**. Sequential decoding never pays the
group-of-pictures cost — each frame follows the one before it — so playback's
28× is what the *smaller picture* buys and nothing else.

The gap between 28× and 72× is therefore what **all-intra** buys, and it is
roughly two and a half times again on top of the resolution. A jump into the
middle of a one-second group costs every frame back to the last keyframe; in an
all-intra file it costs one. That is why a proxy is written intra-frame rather
than simply small, and why a "proxy" that was merely a downscaled long-GOP
re-encode would leave most of the benefit on the table.

It is also the number that made a bug visible. Before the seek unit was fixed —
see the [decoder's own note](#a-third-bug-this-suite-caught) — every seek landed
at the start of the file, so this table read the same for both and the all-intra
half of the benefit was invisible.

### Building one

The price of the two tables above, paid once per file while the editor carries
on:

| Size                  |  90 frames of 1080p | Per frame | Against real time |
|-----------------------|--------------------:|----------:|------------------:|
| Half (960×540)        |             1.60 s  |   17.8 ms |             1.7×  |
| Quarter (480×270)     |             1.03 s  |   11.5 ms |             2.6×  |

So on four cores with no GPU, proxying an hour of 1080p footage at a quarter
takes a little over twenty minutes — and the editor is usable throughout,
because it runs on its own thread. The floor is the decode of the original,
which both rows pay in full: the difference between them is only the scale and
the encode.

### A third bug this suite caught

Writing the scrub table is what turned up a seek that had never worked.
`VideoDecoder::seek` converted its target into the **stream's** time base and
handed that to `avformat_seek_file`, which is called with a stream index of -1
and documents its timestamp as being in `AV_TIME_BASE` units — microseconds. For
the fixtures, whose time base is 1/15360, a request for 1.667 seconds asked for
0.0002 of one.

It produced no wrong frames, which is why none of the seek tests caught it: a
seek that lands too early is still *before* the target, and the decode that
follows walks forward to exactly the right frame. What it produced was a seek
that did nothing — every backward scrub decoding the file from its beginning,
at a cost that grew with how far into the footage the user had got rather than
with how far they had moved.

The two regression tests added with the fix assert on **where the seek put the
reader** rather than on which frame came back, which is the thing the existing
tests could not see.

## Live measurements

The editor's own overlay reports what it is doing, measured the same way. From
the running application playing the demo project on the same machine, software
rasteriser included:

```
FPS       55.5
frame     18.01 ms   p95  26.37
engine     0.02 ms
decode     0.23 ms
scale      0.14 ms
upload     0.17 ms
composite  0.44 ms
ui         1.01 ms
dropped    1  (0.4%)
frames     140  97% hit
```

Note that `frame` is far larger than the sum of the stages: the rest is egui's
own tessellation and paint, plus presentation. That is exactly why `frame` is
measured as the wall-clock interval between repaints rather than as the sum of
what we instrumented — an early version divided by the engine span alone and
cheerfully reported 610,128 FPS.

## What is not benchmarked yet

Named because their absence is a gap, not because they are unimportant:

- Seeking through 4K footage. The proxy suite generates its own 1080p source
  rather than relying on the committed fixtures, which are small deliberately;
  the same trick would extend to 4K, and 4K is where a proxy earns most
- Proxies of **long** files. The build figures are throughput per frame and
  should hold, but nothing here has transcoded an hour to find out
- Effect chains **through the render cache**. A pass is measured on its own and
  the cache is measured without effects; what is not measured is a realistic
  edit where a chain is re-run on one parameter change and the passes before it
  are hits — which is exactly where the cache should matter most
- Effects at 4K, where the decision to run a chain at the source's own
  resolution rather than the canvas's is at its most expensive
- The animation editor as an interaction: the property evaluation under it is
  measured, the drawing of a few hundred keyframes and a sampled curve is not
- Thumbnail generation (not written yet)
- Timeline scrolling and zoom as interactions, as opposed to the queries
  underneath them
- The audio device itself. Mixing, metering and the ring are measured; the
  latency and underrun behaviour of a real sound card are not, because this
  container has none.

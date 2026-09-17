# Benchmarks

Every number here was measured with `cargo bench`. Nothing is estimated.

Run them yourself:

```sh
cargo bench                                   # everything
cargo bench -p ve-core --bench timeline       # edit model
cargo bench -p ve-project --bench project_io  # save and load
cargo bench -p ve-render --bench compositing  # GPU
cargo bench -p ve-media  --bench waveforms    # audio analysis and display
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

- Seeking through 4K footage (no 4K fixture; the committed fixtures are small
  deliberately)
- Effect-heavy compositions (no effects yet), which is where the render cache
  starts to matter most
- Export (not written yet)
- Thumbnail generation (not written yet)
- Timeline scrolling and zoom as interactions, as opposed to the queries
  underneath them

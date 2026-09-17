# Benchmarks

Every number here was measured with `cargo bench`. Nothing is estimated.

Run them yourself:

```sh
cargo bench                                   # everything
cargo bench -p ve-core --bench timeline       # edit model
cargo bench -p ve-project --bench project_io  # save and load
cargo bench -p ve-render --bench compositing  # GPU
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

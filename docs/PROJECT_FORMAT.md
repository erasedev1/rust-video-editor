# The `.verge` project format

Version 1.

## Shape

A project file is pretty-printed JSON with a small envelope around the edit
model:

```json
{
  "format": "verge-project",
  "version": 1,
  "app_version": "0.1.0",
  "saved_at": "2026-09-16T20:24:57Z",
  "saved_at_unix": 1789590297,
  "project": { ... }
}
```

The envelope fields come first and are all plain scalars, so a reader can
establish what it is holding before it understands the payload. That is what
makes migration possible at all.

## Why JSON, and why pretty-printed

Projects are small next to the media they point at. Being diffable, greppable
and repairable by hand is worth far more than the bytes: version control works
on them as-is, and a project damaged by a bug can be rescued with a text editor.

Saving twice with no edits produces byte-identical output apart from the
timestamp, so an unchanged project does not churn a diff.

## Media is referenced, never embedded

Each asset stores both an absolute `path` and, when the media sits under the
project's directory, a `relative_path`. On load the relative form is preferred,
so moving or sharing a project folder relinks it automatically.

A file that cannot be found does not fail the open. The asset is marked offline,
clips referencing it are kept and drawn as offline, and the user is told. Losing
the edit because the media moved would be the worse failure.

## Time in the file

Every time is a plain integer of ticks (282,240,000 per second) and every frame
rate is a `{num, den}` pair. Nothing is stored as floating-point seconds, so a
save/load round trip is exact at any rate, NTSC included.

```json
"rate": { "num": 30000, "den": 1001 },
"timeline_start": 0,
"duration": 846720000
```

## Durability

The primary file is never written in place:

1. Write to a temporary file in the same directory.
2. Flush and `fsync` it.
3. Rename any existing file to `<name>.bak`.
4. Rename the temporary over the target.
5. `fsync` the directory.

Both renames are atomic, so a crash at any point leaves the target holding
either the complete previous version or the complete new one. The only window in
which the target does not exist is between steps 3 and 4, and `load` recovers
from exactly that case by falling back to the backup — as it also does when the
primary file is damaged.

## Autosave

An autosave is written beside the project as `<name>.autosave`, using the same
atomic write. It is never written over the project itself, so it cannot destroy
a deliberate save.

On open, `recovery_candidate` compares the `saved_at_unix` recorded *inside*
each file — not the filesystem mtime, which a copy or a checkout can rewrite —
and offers the autosave when it is newer.

An explicit save discards the autosave, because at that moment it holds nothing
the project file does not.

## Versioning and migration

Migrations run on the raw JSON tree *before* deserialisation, so an old file
never has to satisfy the current Rust types. Each step moves the document from
one version to the next and they are applied in sequence, which means one step
per format change regardless of how old the file is.

A file from a **newer** version is refused with a message naming both versions,
rather than being read partially. A gap in the migration table is an error, not
a silent skip.

Version 1 was the first released format. Version 2 changed a clip's `asset`
field into a tagged `source` — `{"asset": 3}` or `{"composition": 7}` — because a
clip can now hold a composition as readily as a file. That is a shape version 1
readers cannot understand, so it is a version rather than a defaulted field, and
`migrate::clip_asset_to_source` is the one step that performs it. A version 1
file therefore opens, reports the upgrade, and writes version 2 the next time it
is saved.

A migration that finds a clip already carrying a `source` leaves it alone rather
than overwriting it, so a hand-edited or partially upgraded file migrates to
something coherent instead of losing the composition it named.

An **additive** field does not need a version bump at all: a new field that
deserialises from a default reads an older file correctly, and an older build
ignores it when reading a newer one. The clip `blend` field is the worked example,
with a test that loads a document without it and asserts the mode comes back as
`normal`. A bump is for a change that would make an older document mean something
different — a renamed field, a changed unit, a restructured tree.

A clip's `source` gained a third shape in Phase 7 —
`{"multicam": {"group": 12, "angle": 14}}` — alongside `{"asset": 3}` and
`{"composition": 7}`. That is **additive for reading**: every version 2 document
still means exactly what it meant, which is the test for whether a bump is
needed. A version 2 file written by this build can carry a multicam source that
an older build will not understand, and the envelope version is not what would
tell it so; that is the known cost of not bumping, and it is the same cost every
new enum variant carries in a format this young.

Multicam groups live in a `multicams` array on the project, omitted entirely
when there are none. An angle is `{id, name, asset, offset, enabled}`, where
`offset` is where group time zero falls inside that camera's own media.

Everything audio added in Phase 4 is additive, and still at version 2:

| Field                        | Where            | Missing means |
|------------------------------|------------------|---------------|
| `audio.fade_in` / `fade_out` | clip, comp layer | no fade       |
| `volume`                     | track            | unity gain    |
| `pan`                        | track            | centre        |

A fade is `{"length": <ticks>, "curve": "linear" | "equal_power" | "smooth"}`
and is **omitted entirely** when its length is zero, which is the overwhelming
case — a clip with no fades writes no fade fields at all, and a test asserts
that. Track `volume` and `pan` are plain numbers; both are clamped when they are
*read*, so a hand-edited file cannot push a negative gain or an out-of-range pan
into the mixer.

Everything Phase 5 added is additive too, and still at version 2:

| Field                | Where                | Missing means                     |
|----------------------|----------------------|-----------------------------------|
| `motion_blur`        | clip, comp layer     | not blurred                       |
| `motion_blur`        | sequence, comp settings | 180° shutter, twelve samples   |

A clip's switch is a bare boolean and the canvas's is
`{"enabled": true, "shutter_angle": 180.0, "samples": 12}`. The two defaults
disagree on purpose: the canvas shutter comes back **open** so that turning blur
on for a clip needs one click rather than two, and the clip switch comes back
**off** so that a project written before any of this existed draws exactly what
it drew. A test loads a document with both fields stripped and asserts that pair.

Keyframes were always part of the format and are unchanged by this phase: a
`Property` writes its `keyframes` array only when it has one, and each keyframe
carries `{"time": <ticks>, "value": …, "interpolation": {"mode": …}}`. A
hand-shaped curve is `{"mode": "bezier", "x1": …, "y1": …, "x2": …, "y2": …}`,
and the loader re-establishes the sort and uniqueness invariants on the way in,
so a hand-edited file cannot leave a property with two keyframes at one instant.

## Robustness

The loader treats the file as untrusted. It repairs what it can and reports the
rest as warnings, keeping the project usable:

- Clips are re-sorted; overlaps are reported, not discarded
- Clips referencing missing assets are kept and reported
- The ID allocator is advanced past every ID seen in the file — **including IDs
  only referenced**, so an orphaned reference can never later collide with a
  newly imported asset and silently bind to the wrong media
- Unknown fields from a newer build are ignored rather than rejected

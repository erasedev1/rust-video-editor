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

Version 1 is the first released format, so nothing needs migrating yet. The
pipeline exists and is tested so that the first real format change is a one-line
addition rather than new machinery.

## Robustness

The loader treats the file as untrusted. It repairs what it can and reports the
rest as warnings, keeping the project usable:

- Clips are re-sorted; overlaps are reported, not discarded
- Clips referencing missing assets are kept and reported
- The ID allocator is advanced past every ID seen in the file — **including IDs
  only referenced**, so an orphaned reference can never later collide with a
  newly imported asset and silently bind to the wrong media
- Unknown fields from a newer build are ignored rather than rejected

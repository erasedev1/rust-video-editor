# Releasing

Releases are built by GitHub Actions. Pushing a version tag builds every
platform and publishes the archives to the repository's Releases page.

## Cutting a release

1. Check [CI](../../actions/workflows/ci.yml) is green on the commit you intend
   to release. The release workflow builds but does not test, so this is the
   step that establishes the code works.
2. Set the version in the workspace `Cargo.toml` under `[workspace.package]`,
   commit it, and make sure `Cargo.lock` is committed too — the release build
   uses `--locked` and will fail if the lockfile is stale.
3. Tag and push:

   ```sh
   git tag v0.2.0
   git push origin v0.2.0
   ```

A tag containing a hyphen — `v0.2.0-rc1` — is published as a pre-release.

## Rehearsing without publishing

Run the **Release** workflow manually from the Actions tab. A manual run builds
and packages every platform and attaches the archives to the workflow run, but
stops before touching the Releases page. Download them, check they start, then
tag for real.

## What gets built

| Platform              | Runner         | Archive                                |
|-----------------------|----------------|----------------------------------------|
| Linux x86-64          | `ubuntu-24.04` | `verge-<version>-linux-x86_64.tar.gz`  |
| Windows x86-64        | `windows-2022` | `verge-<version>-windows-x86_64.zip`   |
| macOS (Apple silicon) | `macos-14`     | `verge-<version>-macos-aarch64.tar.gz` |

There is no Intel Mac build. GitHub has retired its Intel macOS runners, and a
`macos-13` job is not rejected — it simply queues until something cancels it,
which is how this was found: the first rehearsal sat for 47 minutes without ever
being assigned a runner while the other three finished in under seven.

Adding Intel back would mean cross-compiling from the Apple silicon runner.
Because FFmpeg is linked dynamically, that needs x86-64 FFmpeg dylibs on an
arm64 host as well as the Rust target, so it is real work rather than an extra
matrix row. Intel Mac users can build from source in the meantime.

Every platform is attempted even when one fails, so a single run reports the
state of all four. The publish step runs only if all of them succeeded: a
release missing an architecture is worse than no release.

## The FFmpeg version pin

`.github/actions/setup-ffmpeg` is a composite action shared by CI and the
release build, so the two cannot drift apart. It pins FFmpeg 6 on every
platform, because `ffmpeg-next` tracks FFmpeg's major version and the 6.x
bindings will not build against 7 or later:

| Platform | Source                                                     |
|----------|------------------------------------------------------------|
| Linux    | `ubuntu-24.04` apt, which ships FFmpeg 6.1                  |
| macOS    | Homebrew `ffmpeg@6` (keg-only, so `PKG_CONFIG_PATH` is set) |
| Windows  | A pinned GitHub release asset, `ffmpeg-6.1.1-full_build-shared` |

The Linux runner is pinned to `ubuntu-24.04` rather than `ubuntu-latest`
precisely because the FFmpeg version is what matters; a newer image would bring
a newer FFmpeg and break the build.

**To move to a newer FFmpeg**, bump `ffmpeg-next` in `crates/ve-media/Cargo.toml`
to the matching major version and update all three pins together.

## Licensing of the Windows archive

The Windows archive bundles FFmpeg DLLs so it runs with nothing installed. The
pinned build is a **GPL** build of FFmpeg. Verge itself is MIT or Apache-2.0 and
links FFmpeg dynamically, and `LICENSE-FFmpeg.txt` ships in the archive, but
redistributing those DLLs means distributing GPL software alongside ours.

Before a public release, decide deliberately which you want:

- **Keep bundling the GPL build.** Simplest, best experience, and what the
  workflow does today. Comply with the GPL for what you redistribute.
- **Bundle an LGPL build instead.** Requires an LGPL-configured FFmpeg, which no
  vendor publishes at a stable URL for 6.x, so it means building FFmpeg in CI.
- **Stop bundling.** Have Windows users install FFmpeg 6 themselves, matching
  what Linux and macOS already do. Delete the `Copy-Item ...\*.dll` line.

Linux and macOS archives bundle nothing and are unaffected.

## Known limitations

- Binaries link FFmpeg dynamically, so the Linux and macOS archives need FFmpeg
  6 present at run time. Static linking or bundling would make them
  self-contained and is worth doing.
- The Linux build is produced on Ubuntu 24.04 and inherits its glibc floor, so
  it will not run on noticeably older distributions.
- Only the Linux build is exercised regularly. The other three are built by CI
  but not launched by it.

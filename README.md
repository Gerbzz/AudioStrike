# AudioStrike

**by PWDS**

**Shape the sound you want to hear.**

AudioStrike brings track editing and a mix workspace into a Rust desktop project. Keep work in an editable `.scraft` session, use the companion CLI for supported legacy conversion, and export WAV audio for review.

## Highlights

- Arrange and revise tracks in an editable `.scraft` session.
- Balance a project with the retained mix tools and export WAV audio.
- Convert supported legacy projects through `audiostrike-cli` without changing their sources.

## Current status

Pre-alpha source. The Windows binaries compiled and a synthetic command-line project was converted, edited, reopened, and exported as a non-silent WAV. The desktop window, device playback and recording, third-party plugins, accessibility, and broad interchange fidelity remain unverified; no release binary is provided.

## Edit and export a session

The retained editor source has track editing and mix views for shaping a session before export. Save changes in `.scraft` so the project remains editable; rendered WAV is a separate review file. The exercised CLI path converted a supported `.audiostrike` project into a separate session, edited and reopened it, then exported a non-silent WAV. The conversion keeps references to original media, so those files must remain available. The desktop window and audio devices have not had direct acceptance.

## Legacy projects

The earlier AudioStrike preview used a `.audiostrike` directory, which is distinct from `.scraft`. The supported explicit conversion is:

```text
audiostrike-cli migrate-legacy SOURCE.audiostrike OUTPUT.scraft [--current-state-only]
```

It creates a separate project and leaves the source project/media untouched. The new project references the original media by absolute path. Existing undo history requires `--current-state-only` and stays in the old project. Conversion refuses unsupported pan, fades, overlapping clips, mixed-channel tracks, gains, malformed input, and uncertain recovery state; it does not guarantee sample-identical mixing. Keep a copy of the original project and media.

## Build from source

Use Rust 1.95 or newer and a reviewed Cargo dependency cache. The pinned UI Git dependency is `storytold/craft-libs` revision `5c874d51eeb3de70e735c8ce87a59a7dedd47675`.

```powershell
cargo fetch --locked --target x86_64-pc-windows-msvc
cargo test --locked --offline --workspace
cargo build --locked --offline -p soundcraft -p soundcraft-cli --bins
```

The donor MIT/Apache licenses and `NOTICE` are preserved. [UPSTREAM.md](UPSTREAM.md) records source provenance, [ATTRIBUTION.md](ATTRIBUTION.md) separates donor and AudioStrike changes, and [DEPENDENCY_RESOLUTION.md](DEPENDENCY_RESOLUTION.md) records the pinned external UI dependency. Upstream brand assets and app icon are excluded. The About contributor table is empty because upstream contributor metadata is not included in this public source boundary.

## Source and verification notes

AudioStrike adapts the complete [SoundCraft](https://github.com/storytold/soundcraft) Rust source at `480b0b2dd75df388d79746a035652069a9bdcbc6`. The Windows binaries are `audiostrike` and `audiostrike-cli`; retained `soundcraft-*` crate names and `.scraft` projects support compatibility. The workspace passed 612 tests with one ignored. A synthetic CLI project was converted, edited, saved, reopened, and exported as non-silent WAV. The desktop window has not been visually tested.

# AudioStrike

**by PWDS**

**Shape a session, then render the mix.** AudioStrike brings track editing, a mix workspace, editable `.scraft` sessions, and WAV export into a Rust desktop source project.

[Build from source](#build-from-source) · [Legacy conversion](#bring-an-earlier-project-forward) · [Source provenance](UPSTREAM.md)

## Build a mix without losing the project

### Arrange and revise tracks

The retained editor source shows clips, waveforms, track controls, the timeline, and a mix workspace. Work stays in an editable `.scraft` session; a rendered WAV is a separate review file. The source includes a synthesized demo session rather than captured microphone or bundled commercial media.

### Export for review

The tested command-line path converted a supported older project, edited and reopened the new session, and exported a **non-silent WAV**. That bounded result is evidence for the CLI workflow, not for speaker audibility or broad interchange fidelity.

### Bring an earlier project forward

The earlier AudioStrike preview used a `.audiostrike` directory, which is a different format. Use an explicit copy-based conversion:

```text
audiostrike-cli migrate-legacy SOURCE.audiostrike OUTPUT.scraft [--current-state-only]
```

The command leaves the old project and media untouched. The new session refers to that media by absolute path, so keep it available. Existing undo history stays in the old project and requires `--current-state-only`. The converter refuses unsupported pan, fades, overlapping clips, mixed-channel tracks, gains, malformed input, and uncertain recovery state. It does not promise a sample-identical mix.

## Build from source

Use Rust 1.95 or newer and reviewed Cargo dependencies. The UI dependency is pinned to `storytold/craft-libs` revision `5c874d51eeb3de70e735c8ce87a59a7dedd47675`.

```powershell
cargo fetch --locked --target x86_64-pc-windows-msvc
cargo test --locked --offline --workspace
cargo build --locked --offline -p soundcraft -p soundcraft-cli --bins
```

The Windows binaries are `audiostrike` and `audiostrike-cli`; retained `soundcraft-*` crate names and `.scraft` files support source compatibility. A fresh checkout needs space and dependencies before building.

## Current status

This is a **pre-alpha source checkpoint**. The workspace passed 612 tests with one ignored, and the synthetic CLI conversion/edit/reopen/export path produced non-silent WAV. The desktop executable compiled but has **not** been opened or visually accepted. Device playback, recording, third-party plugins, accessibility, general format fidelity, and release packaging remain unverified. No release binary is provided.

AudioStrike adapts the complete [SoundCraft](https://github.com/storytold/soundcraft) Rust source at `480b0b2dd75df388d79746a035652069a9bdcbc6`. The donor MIT/Apache licenses and `NOTICE` remain. [UPSTREAM.md](UPSTREAM.md) records provenance, [ATTRIBUTION.md](ATTRIBUTION.md) distinguishes included donor work from AudioStrike changes, and [DEPENDENCY_RESOLUTION.md](DEPENDENCY_RESOLUTION.md) records the pinned UI dependency. Upstream brand assets and app icon are excluded; a transitive binary notice audit remains before distribution.

Explore the other [PWDS Strike repositories](https://github.com/Gerbzz?tab=repositories&q=Strike).

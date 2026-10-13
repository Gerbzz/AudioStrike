# AudioStrike

**by PWDS**

**Shape the sound you want to hear.**

AudioStrike is a local audio workstation with a desktop editor and a headless editing tool. Work on an editable project, revisit changes, and export audio when a mix is ready for review.

### Highlights

- Open and edit local audio projects.
- Keep an editable session alongside exported sound.
- Use the desktop target or the command-line editor.

## Current status

Pre-alpha source. The Windows binaries compiled and a synthetic command-line project was converted, edited, reopened, and exported as a non-silent WAV. The desktop window, device playback and recording, third-party plugins, accessibility, and broad interchange fidelity remain unverified; no release binary is provided.

## Work with a session

Keep changes in an editable audio project while developing a mix, then export a WAV for review. The command-line editor supports a headless path for conversion and edits; the native desktop target needs its own interaction and audio-device checks before a release.

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

# AudioStrike source preview

AudioStrike is a local desktop audio workstation based on the complete [SoundCraft](https://github.com/storytold/soundcraft) Rust source at commit `480b0b2dd75df388d79746a035652069a9bdcbc6`. The Windows desktop target is `audiostrike`; the headless editor/converter is `audiostrike-cli`. Internal `soundcraft-*` crate names and the donor `.scraft` editable format remain for compatibility. No hosted service or account is required.

**Status: pre-alpha source.** On Windows, the desktop and CLI binaries compiled with the pinned lockfile, and the workspace suite passed 612 tests with one ignored. A synthetic CLI workflow converted a legacy project, edited and saved it, reopened it, and exported a non-silent WAV. The desktop window itself has not been opened or visually tested. Device playback, recording, third-party plugins, accessibility, other operating systems, and broad interchange fidelity are unverified. This repository does not provide a release binary.

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

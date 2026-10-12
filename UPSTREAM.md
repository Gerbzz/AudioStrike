# SoundCraft source provenance

- Upstream: [storytold/soundcraft](https://github.com/storytold/soundcraft), commit `480b0b2dd75df388d79746a035652069a9bdcbc6`.
- This derivative includes the SoundCraft desktop and CLI applications, `crates/*`, workspace manifests and lockfile, and `xtask/*`, with bounded AudioStrike identity, legacy conversion and save/export safety changes. It is a complete DAW source tree rather than an adapter around the older AudioStrike preview.
- Excluded: restricted ArtCraft brand assets, donor app icon and packaging, screenshots, the donor experimental web runner, donor roadmap, and contributor metadata. The selected UI build script supports an empty About-contributor table.
- The original `LICENSE-MIT`, `LICENSE-APACHE` and `NOTICE` are preserved byte for byte. [UPSTREAM_ATTRIBUTION.md](UPSTREAM_ATTRIBUTION.md) retains the donor's historical asset inventory; [ATTRIBUTION.md](ATTRIBUTION.md) describes what this derivative actually includes. The original `NOTICE` also mentions donor brand/icon assets that are excluded here.
- Internal package names and native `.scraft` format remain. The earlier `.audiostrike` format requires explicit copy-based CLI conversion. No first-party main ArtCraft studio source was copied.

# AudioStrike source and dependency attribution

## FilmCraft donor

- Repository: https://github.com/storytold/filmcraft
- Exact commit: `2ff3f5e266838700c292a35aaf762e3d4dcef980`
- Copied file: `crates/audio-dsp/src/resample.rs` to `third_party/filmcraft/resample.rs` without modification.
- SHA-256 of copied source: `766027A9A34918F20A79BA90877189BC44E8426662B57EE057F85BC642DF36E5`.
- License: MIT or Apache-2.0 upstream; the included `third_party/filmcraft/LICENSE-MIT` preserves the MIT notice. No upstream executable, hosted service, asset, icon, or brand file was copied.
- Usage: 44.1 to 48 kHz import conversion only; the current offline mixer is AudioStrike code. The donor's streaming phase behavior is exercised by its included tests and AudioStrike's 44.1 kHz fixture.
- **Audio UI adaptation:** `src/donor/filmcraft_ui.rs` adapts `crates/ui-egui/src/panels/mixer.rs`'s `TAPER`, `db_to_pos`, `pos_to_db` and `crates/ui-egui/src/panels/timeline.rs`'s `timeline_extent`, `max_scroll`, `zoom_about`, and `waveform_display_gain`, all from the same pinned FilmCraft commit. The fader interpolation and anchored zoom logic are retained; AudioStrike changes the view tail from 600 to 30 seconds, replaces FilmCraft time/project types with an audio-only view, and uses its local gain conversion. The original timeline and mixer panels cannot be imported whole because they depend on FilmCraft's video project engine, themes, and commands. The app's actual fader, zoom, and waveform views call these adapted functions.

## Registry crates

`Cargo.lock` pins the exact graph used in the tested build. The cached manifests for `serde 1.0.228`, `serde_json 1.0.149`, and `sha2 0.10.9` declare `MIT OR Apache-2.0` and identify the serde-rs and RustCrypto GitHub repositories. The separate combined Windows normal/build-dependency notice audit is not included in this standalone source checkout. A binary distribution must include a verified notice bundle for its exact contents. This checkout contains no binaries.

The native preview adds `eframe 0.30.0` (MIT OR Apache-2.0, https://github.com/emilk/egui), `rfd 0.15.4` (MIT, https://github.com/PolyMeilex/rfd), `cpal 0.15.3` (Apache-2.0, https://github.com/RustAudio/cpal), and `png 0.18.1` (MIT OR Apache-2.0, https://github.com/image-rs/image-png) for local QA screenshot output. These are pinned for the Windows preview; their binary notice bundle and maintenance review remain release gates.

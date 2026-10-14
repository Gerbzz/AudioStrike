# README screenshot provenance

The three README images are direct `ui.screenshot` PNG captures from a retained native AudioStrike Windows executable on 2026-10-14. They have not been composited, retouched, or redrawn. The artwork is the app's built-in “Midnight Groove” demo, whose audio is synthesized in source code. No microphone, audio device, client recording, donor artwork, or downloaded media was used.

The screenshot executable has SHA-256 `CFB83D035903DA9982E55E2140DD3845F03D196525591F7FA352800EC7C79A71`. Its Cargo depfile points to the publication-candidate source checkout. A raw Git blob comparison of all 252 Rust, manifest, lock, JSON, and text files in that checkout found zero missing and zero differing files in the public AudioStrike repository at the time of capture. A separate, later local foundation tree contains two additional “by PWDS” UI labels and contributor metadata; it was not the build checkout for these screenshots. This is source-match evidence for the retained executable, not a new clean-checkout build or broad release test.

The app ran with `--no-audio --demo --control` on a temporary loopback port. Preferences were disabled and the settings path was isolated to a task-local capture folder. The first screenshot shows the untouched Edit view. For the second, `track.rename` changed track ID 3 from “Kick” to “Pulse Kick”; `session.inspect` confirmed the name. For the third, `mix.volume` set the selected track to −8 dB, `session.inspect` confirmed its `volume_db`, and the app's `window.mix` command opened the Mix view. The session was not saved, played through speakers, or exported during this capture. A separate prior CLI verification established a bounded non-silent WAV export.

| Asset | Size | SHA-256 |
| --- | ---: | --- |
| `audiostrike-edit.png` | 360,386 bytes | `0D6FC7979A8101E4B5C21DBEFC8D66D2180FFAFE59E17EB959D33D8E71901EFC` |
| `audiostrike-track-edit.png` | 363,318 bytes | `6652505F2D400ABEC8EDD364A8EDAF8CC1E684A3A84E62FB0B8CFDE7B52C3010` |
| `audiostrike-mix.png` | 248,759 bytes | `4437D4F28EEA6FAD404D8FC7CDF0032A6398DDD2DADD4E647EDED7355EA0DF86` |

The two task-local capture folders, including tiny logs, totalled 972,525 bytes. Before and after capture, free C: space stayed above 11.36 billion bytes. No builds, downloads, or deletions were performed for these screenshots.

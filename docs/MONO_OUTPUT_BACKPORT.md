# Mono output backport review

Mono devices now receive `(0.5 * left + 0.5 * right)` before clipping. Surround
first uses the existing ITU stereo fold-down. The upstream finite-result check
contains NaN from overflowing opposite surround sides. Equal-width device
mapping, wider device silence, resampling and the mono track pan law retain
their existing behavior.

- AudioStrike base/main: `e0cdcdfdb6448ff63cba0644cc53c8997402a58b`.
- Isolated local branch: `room/audiostrike-mono-output`.
- Upstream: [storytold/soundcraft `516ef2370386782cf20c8c32fb6cebed5595c2ea`](https://github.com/storytold/soundcraft/commit/516ef2370386782cf20c8c32fb6cebed5595c2ea),
  `fix(playback): fold both stereo channels into mono output`, by reoniazx.
- Manually ported the playback logic, documentation and three upstream tests;
  added one direct pending-buffer nonfinite/extreme-value regression.
- The original MIT/Apache licenses, NOTICE and attribution files are retained.

## Dependency compatibility

No manifest or lockfile changes are needed. The code uses existing `AudioState`,
model, audio-io, mix and DSP APIs and standard-library float operations.
Both versions use cpal 0.16; AudioStrike retains locked cpal 0.16.0 and rustfft
6.4.1. The newer upstream playback manifest also contains serde, serde_json and
optional ASIO support, which this fix does not use and does not import.

AudioStrike retains Rust 1.95, edition 2024, the pinned craft-ui revision
`5c874d51eeb3de70e735c8ce87a59a7dedd47675`, egui/eframe 0.36.2 and sha2
0.10.9. The upstream workspace's lower Rust requirement and additional browser
bundle dependency are outside this backport. Locked Windows dependency fetch
and the focused builds succeeded on Rust/Cargo 1.96.1, x86_64-pc-windows-msvc.
Rust 1.95 itself was not tested.

## Verification on 2026-10-21

Tests below were compiled and executed, rather than only built:

| Check | Result |
| --- | --- |
| Four new `mono_device_` regressions against unchanged production code | All four failed as expected |
| Entire playback library after the fix | 18 passed, 0 failed |
| Existing `io::export_safety_tests` | Compiled; 3 executed and failed with OS error 5, Access is denied, in sandbox temporary-file operations |
| Playback formatting | Passed |
| Diff whitespace with existing CRLF accepted | Passed |

The new tests cover right-only/left-only stereo, equal/opposite polarity,
averaging before clipping, 1/37/512-frame callbacks, centered mono level, every
5.1 channel, surround overflow, NaN on either stereo side, opposite infinities
and extreme finite values. Existing playback tests also cover stereo surround
fold-down and direct multichannel device mapping.

The engine library test binary compiled all 146 tests, including seven legacy
conversion tests. Those seven legacy tests were not executed. Save/export
failures were `new_export_preserves_occupied_files_and_session_media` (hard-link
creation), `save_keeps_last_good_session_when_backup_destination_is_blocked`
(initial session save), and
`save_refuses_an_occupied_recovery_temp_without_changing_either_file` (the later
permitted replacement). The denied operations were not retried. The failed
assertions do not establish a product regression or a passing safety check.

Save/export and legacy conversion source/tests, GUI/apps, assets, all manifests,
the lockfile, and original license/attribution files are byte-unchanged from the
base. No real sessions or media were opened. Hosted Windows CI runs the four
mono regressions, entire playback library, save/export refusals, legacy
conversion preservation and full workspace tests on the exact PR head.
Those results verify source on the hosted runner; they do not establish Room
acceptance or hardware playback. Other-platform and hardware checks remain
unverified. Only focused one-job local builds ran,
with optimization/debug output reduced; over 2,060 GiB remained free, above the
20 GiB reserve. The compiler reported an incremental-cache hard-link fallback
to copying during the playback build; all playback tests still executed.

The user subsequently authorized publication, CI, independent review and merge
after the required checks pass. PR checks and the review record provide the
exact tested head and delivery outcome. Local OS-denied operations are not
retried or treated as passing because hosted checks succeed. Release and
deployment remain outside this change.

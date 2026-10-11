# AudioStrike Windows engineering preview

AudioStrike is a local Rust multitrack audio editor. Its current Windows native preview opens a portable `.audiostrike` project, imports WAV onto separate timeline tracks, shows waveforms, plays through the default output device, edits clip position and fades, adjusts track volume/pan/mute/solo, saves each edit, reopens it, and renders stereo 48 kHz 24-bit PCM WAV. Clip split at the playhead, undo/redo, anchored timeline zoom, and horizontal scrolling are available. This is an engineering checkpoint, not a first-release workstation.

Build and launch in PowerShell from this directory:

```powershell
$env:CARGO_BUILD_JOBS='1'
$env:CARGO_TARGET_DIR='..\target-audiostrike'
cargo test --locked --features desktop
cargo run --locked --features desktop --bin audiostrike-desktop
```

A fresh checkout needs registry access for its first dependency download. After the registry cache is populated, add `--offline` if an offline build is needed.

Use **New** to create a `.audiostrike` folder, **Import WAV** to add a track and clip, drag a clip to move it, select a clip for precise start/fade/split controls, then **Bounce WAV**. Space toggles playback, Ctrl+S saves, Ctrl+Z undoes, and Ctrl+Y or Ctrl+Shift+Z redoes when text editing is not focused. Every successful edit saves. The project contains `project.json` and canonical float WAV files in `media/`; the source import stays untouched. The app shows errors in its status bar and refuses invalid inputs.

Local QA helpers: `--demo PATH.audiostrike` creates two generated PCM stems; `--project PATH.audiostrike` opens a project; `--capture FILE.png` requests a screenshot from the running app and exits; `--probe-audio` checks the default output config; `--smoke-audio PATH.audiostrike` runs 500 ms of output and checks that the callback advanced. The checked-in tests generate their own fixtures; QA captures are not included in this source checkout. The command line core remains available with `cargo run --locked --bin audiostrike --` and prints the available commands.

The native timeline and mixer use adapted, attributed logic from pinned FilmCraft source. WAV import and playback-to-device rate conversion use its pinned resampler. Exact upstream paths, commit, license, local copies, and adaptations are in [`THIRD_PARTY.md`](THIRD_PARTY.md).

Current limits: WAV extensible and compressed input are rejected; 16/24-bit PCM and float 32-bit WAV at 44.1/48 kHz are accepted. Import still buffers at most 256 MiB. Playback and 24-bit export stream project media in bounded 4,096-frame blocks; the callback reads from a lock-free frame ring while a worker fills it. The output adapter selects a supported device configuration, converts 48 kHz project audio when needed, maps channels and sample formats, and permits a fresh retry after device loss. A recording writer accepts caller-supplied 48 kHz float frames and writes a valid WAV; its synthetic-input test does not open a microphone, and there is no recording UI or device input path yet. Project save refuses existing temp/backup sidecars instead of replacing recovery data; malformed media frame counts are rejected on open and in the streaming reader.

Bounce requires a new `.wav` destination outside the project tree. It uses unique staging and no-clobber publication, refuses occupied files and tested project/recovery/hard-link/symlink aliases, and removes staging after a late render error. Sample peaks above full scale still fail export. The frozen Windows source passed 34 desktop-feature tests, including six export-safety and seven deterministic output-adapter cases; a separate CLI replay and independent recheck confirmed the tested no-clobber paths. Concurrent path-swap attacks, native OS save-dialog interaction, real hardware loss/recovery, audible fidelity, pointer editing, and screen-reader behavior remain unverified. There is no normalization, dither, automation, or plugin hosting. Windows is the only tested platform.

Planning history and QA captures are not included in this standalone source checkout.

## Source terms

No first-party license grant is stated for this app in this checkout. `THIRD_PARTY.md` records attribution for identified upstream material; the included donor license files, where present, apply to that material.

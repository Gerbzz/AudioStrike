use audiostrike::Project;
use std::path::Path;

fn usage() -> &'static str {
    "AudioStrike commands:\n  new PROJECT.audiostrike TEMPO\n  import PROJECT.audiostrike INPUT.wav\n  track PROJECT.audiostrike NAME\n  clip PROJECT.audiostrike TRACK_ID MEDIA_ID START_FRAME\n  move PROJECT.audiostrike CLIP_ID START_FRAME\n  split PROJECT.audiostrike CLIP_ID FRAME\n  undo PROJECT.audiostrike\n  redo PROJECT.audiostrike\n  status PROJECT.audiostrike\n  bounce PROJECT.audiostrike OUTPUT.wav"
}
fn number(value: Option<&String>, label: &str) -> Result<u64, String> {
    value
        .ok_or_else(|| format!("missing {label}"))?
        .parse()
        .map_err(|_| format!("invalid {label}"))
}
fn run(args: &[String]) -> Result<(), String> {
    let command = args.first().ok_or_else(|| usage().to_string())?;
    let path = Path::new(args.get(1).ok_or_else(|| usage().to_string())?);
    if command == "new" {
        let tempo: u16 = number(args.get(2), "tempo")?
            .try_into()
            .map_err(|_| "tempo out of range".to_string())?;
        Project::create(path, tempo).map_err(|e| e.to_string())?;
        println!("Created {}", path.display());
        return Ok(());
    }
    let mut project = Project::open(path).map_err(|e| e.to_string())?;
    match command.as_str() {
        "import" => {
            let source = args.get(2).ok_or_else(|| "missing INPUT.wav".to_string())?;
            let id = project
                .import_wav(Path::new(source))
                .map_err(|e| e.to_string())?;
            project.save().map_err(|e| e.to_string())?;
            println!("Imported media {id}");
        }
        "track" => {
            let name = args
                .get(2)
                .ok_or_else(|| "missing track name".to_string())?;
            let id = project.add_track(name).map_err(|e| e.to_string())?;
            project.save().map_err(|e| e.to_string())?;
            println!("Added track {id}");
        }
        "clip" => {
            let track = number(args.get(2), "track ID")?;
            let media = number(args.get(3), "media ID")?;
            let start = number(args.get(4), "start frame")?;
            let id = project
                .place_clip(track, media, start)
                .map_err(|e| e.to_string())?;
            project.save().map_err(|e| e.to_string())?;
            println!("Placed clip {id}");
        }
        "move" | "split" => {
            let id = number(args.get(2), "clip ID")?;
            let frame = number(args.get(3), "frame")?;
            if command == "move" {
                project.move_clip(id, frame).map_err(|e| e.to_string())?;
            } else {
                let new_id = project.split_clip(id, frame).map_err(|e| e.to_string())?;
                println!("Created right clip {new_id}");
            }
            project.save().map_err(|e| e.to_string())?;
        }
        "undo" => {
            project.undo().map_err(|e| e.to_string())?;
            project.save().map_err(|e| e.to_string())?;
        }
        "redo" => {
            project.redo().map_err(|e| e.to_string())?;
            project.save().map_err(|e| e.to_string())?;
        }
        "status" => {
            let session = project.session();
            println!(
                "{}: {} BPM, {} Hz",
                path.display(),
                session.tempo_bpm,
                session.sample_rate
            );
            for asset in &session.media {
                println!(
                    "  media {}: {} frames, {} channel(s)",
                    asset.id, asset.frames, asset.channels
                );
            }
            for track in &session.tracks {
                println!("  track {}: {}", track.id, track.name);
                for clip in &track.clips {
                    println!(
                        "    clip {}: media {}, start {}, source {}, length {}",
                        clip.id, clip.media_id, clip.start, clip.source_in, clip.len
                    );
                }
            }
            for id in project.offline_media() {
                eprintln!("Offline media: {id}");
            }
        }
        "bounce" => {
            let output = args
                .get(2)
                .ok_or_else(|| "missing OUTPUT.wav".to_string())?;
            project
                .render_wav(Path::new(output))
                .map_err(|e| e.to_string())?;
            println!("Rendered {output}");
        }
        _ => return Err(usage().to_string()),
    }
    Ok(())
}
fn main() {
    if let Err(error) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

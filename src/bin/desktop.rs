use audiostrike::device::{AudioPlayer, probe_output};
use audiostrike::donor::filmcraft_ui::{
    TimelineView, db_to_pos, max_scroll, pos_to_db, waveform_display_gain, zoom_about,
};
use audiostrike::stream::StreamingMixer;
use audiostrike::{Clip, Project, Track, WavData, read_wav, write_wav_float};
use eframe::egui::{self, Align, Align2, Color32, FontId, Rect, Sense, Stroke, pos2, vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const RATE: f64 = 48_000.0;
const HEADER_W: f32 = 176.0;
const RULER_H: f32 = 38.0;
const ROW_H: f32 = 92.0;
const BG: Color32 = Color32::from_rgb(17, 22, 30);
const PANEL: Color32 = Color32::from_rgb(25, 33, 43);
const GRID: Color32 = Color32::from_rgb(52, 64, 76);
const TEXT: Color32 = Color32::from_rgb(226, 235, 240);
const MUTED: Color32 = Color32::from_rgb(145, 159, 170);
const ACCENT: Color32 = Color32::from_rgb(105, 211, 187);
const CLIP: Color32 = Color32::from_rgb(47, 96, 109);
const CONTROL: Color32 = Color32::from_rgb(39, 52, 63);

fn button(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).color(TEXT).size(13.0))
        .fill(CONTROL)
        .stroke(Stroke::new(1.0_f32, GRID))
}

fn accent_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).color(BG).strong().size(13.0))
        .fill(ACCENT)
        .stroke(Stroke::NONE)
}

fn install_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    visuals.widgets.inactive.fg_stroke.color = TEXT;
    visuals.widgets.hovered.fg_stroke.color = TEXT;
    visuals.widgets.active.fg_stroke.color = TEXT;
    visuals.selection.bg_fill = Color32::from_rgb(42, 91, 90);
    visuals.selection.stroke.color = ACCENT;
    ctx.set_visuals(visuals);
}

#[derive(Clone, Copy)]
struct Peak {
    min: f32,
    max: f32,
}

enum Action {
    New(PathBuf),
    Open(PathBuf),
    Import(PathBuf),
    Bounce(PathBuf),
    AddTrack(String),
    Move(u64, u64),
    Split(u64, u64),
    Gain(u64, f64),
    Pan(u64, f32),
    Mute(u64, bool),
    Solo(u64, bool),
    Fades(u64, u64, u64),
    Undo,
    Redo,
    Play,
    Stop,
    Seek(u64),
    Select(u64, u64),
}

struct AudioStrikeApp {
    project: Option<Project>,
    selected_track: Option<u64>,
    selected_clip: Option<u64>,
    playhead_frame: u64,
    player: Option<AudioPlayer>,
    peaks: HashMap<u64, Vec<Peak>>,
    source_peaks: HashMap<u64, f32>,
    view: TimelineView,
    new_tempo: u16,
    track_name: String,
    status: String,
    capture_path: Option<PathBuf>,
    capture_requested: bool,
    frame_count: usize,
}
impl AudioStrikeApp {
    fn new(project: Option<Project>, capture_path: Option<PathBuf>) -> Self {
        let mut app = Self {
            project,
            selected_track: None,
            selected_clip: None,
            playhead_frame: 0,
            player: None,
            peaks: HashMap::new(),
            source_peaks: HashMap::new(),
            view: TimelineView {
                target_pps: 110.0,
                target_scroll: 0.0,
            },
            new_tempo: 120,
            track_name: "New Track".into(),
            status: "Ready · local project".into(),
            capture_path,
            capture_requested: false,
            frame_count: 0,
        };
        app.reload_project_ui();
        app
    }
    fn reload_project_ui(&mut self) {
        self.peaks.clear();
        self.source_peaks.clear();
        if let Some(project) = &self.project {
            self.selected_track = project.session().tracks.first().map(|t| t.id);
            self.selected_clip = project
                .session()
                .tracks
                .first()
                .and_then(|t| t.clips.first())
                .map(|c| c.id);
            for asset in &project.session().media {
                let path = project
                    .root()
                    .join("media")
                    .join(format!("{}.wav", asset.id));
                if let Ok(wav) = read_wav(&path) {
                    let (bins, peak) = compute_peaks(&wav);
                    self.peaks.insert(asset.id, bins);
                    self.source_peaks.insert(asset.id, peak);
                }
            }
        } else {
            self.selected_track = None;
            self.selected_clip = None;
        }
    }
    fn save_after(&mut self, message: &str) {
        if let Some(project) = &mut self.project {
            match project.save() {
                Ok(()) => self.status = message.into(),
                Err(e) => self.status = format!("Save failed: {e}"),
            }
        }
    }
    fn action(&mut self, action: Action) {
        match action {
            Action::New(path) => {
                self.player = None;
                match Project::create(&path, self.new_tempo) {
                    Ok(project) => {
                        self.project = Some(project);
                        self.reload_project_ui();
                        self.status = format!("Created {}", path.display());
                    }
                    Err(e) => self.status = format!("New project: {e}"),
                }
            }
            Action::Open(path) => {
                self.player = None;
                match Project::open(&path) {
                    Ok(project) => {
                        self.project = Some(project);
                        self.reload_project_ui();
                        self.status = format!("Opened {}", path.display());
                    }
                    Err(e) => self.status = format!("Open project: {e}"),
                }
            }
            Action::Import(path) => {
                self.player = None;
                let result = (|| {
                    let project = self
                        .project
                        .as_mut()
                        .ok_or("create or open a project first".to_string())?;
                    let asset = project.import_wav(&path).map_err(|e| e.to_string())?;
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Audio");
                    let track = project.add_track(name).map_err(|e| e.to_string())?;
                    let clip = project
                        .place_clip(track, asset, 0)
                        .map_err(|e| e.to_string())?;
                    project.save().map_err(|e| e.to_string())?;
                    Ok::<_, String>((track, clip))
                })();
                match result {
                    Ok((track, clip)) => {
                        self.reload_project_ui();
                        self.selected_track = Some(track);
                        self.selected_clip = Some(clip);
                        self.status = format!("Imported {} to new track", path.display());
                    }
                    Err(e) => self.status = format!("Import: {e}"),
                }
            }
            Action::Bounce(path) => {
                if let Some(project) = &self.project {
                    match project.render_wav(&path) {
                        Ok(()) => self.status = format!("Rendered 24-bit WAV: {}", path.display()),
                        Err(e) => self.status = format!("Bounce: {e}"),
                    }
                }
            }
            Action::AddTrack(name) => {
                let result = self.project.as_mut().map(|p| p.add_track(&name));
                match result {
                    Some(Ok(id)) => {
                        self.selected_track = Some(id);
                        self.save_after("Track added");
                    }
                    Some(Err(e)) => self.status = e.to_string(),
                    None => {}
                }
            }
            Action::Move(id, start) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.move_clip(id, start));
                self.commit_result(result, "Clip moved");
            }
            Action::Split(id, frame) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.split_clip(id, frame));
                match result {
                    Some(Ok(right)) => {
                        self.selected_clip = Some(right);
                        self.save_after("Clip split");
                    }
                    Some(Err(e)) => self.status = format!("Split: {e}"),
                    None => {}
                }
            }
            Action::Gain(id, db) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.set_track_gain_db(id, db));
                self.commit_result(result, "Track volume set");
            }
            Action::Pan(id, pan) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.set_track_pan(id, pan));
                self.commit_result(result, "Track pan set");
            }
            Action::Mute(id, mute) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.set_track_mute(id, mute));
                self.commit_result(result, "Track mute set");
            }
            Action::Solo(id, solo) => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.set_track_solo(id, solo));
                self.commit_result(result, "Track solo set");
            }
            Action::Fades(id, start, end) => {
                self.player = None;
                let result = self
                    .project
                    .as_mut()
                    .map(|p| p.set_clip_fades(id, start, end));
                self.commit_result(result, "Clip fades set");
            }
            Action::Undo => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.undo());
                self.commit_result(result, "Undo saved");
                self.reload_project_ui();
            }
            Action::Redo => {
                self.player = None;
                let result = self.project.as_mut().map(|p| p.redo());
                self.commit_result(result, "Redo saved");
                self.reload_project_ui();
            }
            Action::Play => {
                self.player = None;
                if let Some(project) = &self.project {
                    match StreamingMixer::open(project)
                        .map_err(|e| e.to_string())
                        .and_then(|mixer| AudioPlayer::start_streaming(mixer, self.playhead_frame as usize)) {
                        Ok(player) => {
                            self.status = format!("Playing via {}", player.device_name);
                            self.player = Some(player);
                        }
                        Err(e) => self.status = format!("Playback: {e}"),
                    }
                }
            }
            Action::Stop => {
                if let Some(player) = self.player.take() {
                    self.playhead_frame = player.frame() as u64;
                }
                self.status = "Stopped".into();
            }
            Action::Seek(frame) => {
                self.player = None;
                self.playhead_frame = frame;
            }
            Action::Select(track, clip) => {
                self.selected_track = Some(track);
                self.selected_clip = Some(clip);
            }
        }
    }
    fn commit_result(
        &mut self,
        result: Option<std::result::Result<(), audiostrike::Error>>,
        label: &str,
    ) {
        match result {
            Some(Ok(())) => self.save_after(label),
            Some(Err(e)) => self.status = e.to_string(),
            None => {}
        }
    }
    fn selected(&self) -> (Option<Track>, Option<Clip>) {
        let track = self
            .project
            .as_ref()
            .and_then(|p| {
                p.session()
                    .tracks
                    .iter()
                    .find(|t| Some(t.id) == self.selected_track)
            })
            .cloned();
        let clip = track
            .as_ref()
            .and_then(|t| t.clips.iter().find(|c| Some(c.id) == self.selected_clip))
            .cloned();
        (track, clip)
    }
    fn timeline(&mut self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(project) = &self.project else {
            ui.vertical_centered(|ui| {
                ui.add_space(100.0);
                ui.heading("Start a local session");
                ui.label("Create a project, then import WAV stems to see an editable timeline.");
            });
            return;
        };
        let tracks = project.session().tracks.clone();
        let duration = tracks
            .iter()
            .flat_map(|t| &t.clips)
            .map(|c| c.start + c.len)
            .max()
            .unwrap_or(0) as f64
            / RATE;
        let width = ui.available_width().max(400.0);
        let total_h = RULER_H + (tracks.len().max(1) as f32) * ROW_H;
        let (canvas, _) = ui.allocate_exact_size(vec2(width, total_h), Sense::hover());
        let painter = ui.painter_at(canvas);
        painter.rect_filled(canvas, 0.0, BG);
        let timeline_x = canvas.min.x + HEADER_W;
        let timeline_w = (canvas.width() - HEADER_W).max(1.0);
        let ruler = Rect::from_min_max(
            pos2(timeline_x, canvas.min.y),
            pos2(canvas.max.x, canvas.min.y + RULER_H),
        );
        painter.rect_filled(ruler, 0.0, PANEL);
        let header = Rect::from_min_max(canvas.min, pos2(timeline_x, canvas.min.y + RULER_H));
        painter.rect_filled(header, 0.0, Color32::from_rgb(31, 42, 52));
        painter.text(
            header.left_center() + vec2(16.0, 0.0),
            Align2::LEFT_CENTER,
            "TRACKS",
            FontId::proportional(11.0),
            MUTED,
        );
        let start_sec = self.view.target_scroll;
        let end_sec = start_sec + timeline_w as f64 / self.view.target_pps;
        let step: i64 = if self.view.target_pps >= 65.0 {
            1
        } else if self.view.target_pps >= 18.0 {
            5
        } else {
            10
        };
        for s in (start_sec.floor() as i64 - 1).max(0)..=(end_sec.ceil() as i64 + 1) {
            if s % step != 0 {
                continue;
            }
            let x = timeline_x + ((s as f64 - start_sec) * self.view.target_pps) as f32;
            if x < timeline_x || x > canvas.max.x {
                continue;
            }
            painter.line_segment(
                [pos2(x, ruler.min.y + 20.0), pos2(x, canvas.max.y)],
                Stroke::new(1.0_f32, GRID),
            );
            painter.text(
                pos2(x + 4.0, ruler.min.y + 10.0),
                Align2::LEFT_CENTER,
                format_time(s as f64),
                FontId::monospace(10.0),
                MUTED,
            );
        }
        let ruler_response = ui.interact(ruler, ui.id().with("ruler"), Sense::click());
        if ruler_response.clicked() {
            if let Some(pointer) = ruler_response.interact_pointer_pos() {
                let seconds =
                    (start_sec + (pointer.x - timeline_x) as f64 / self.view.target_pps).max(0.0);
                actions.push(Action::Seek((seconds * RATE).round() as u64));
            }
        }
        for (row, track) in tracks.iter().enumerate() {
            let top = canvas.min.y + RULER_H + row as f32 * ROW_H;
            let row_rect =
                Rect::from_min_max(pos2(canvas.min.x, top), pos2(canvas.max.x, top + ROW_H));
            painter.rect_filled(
                row_rect,
                0.0,
                if row % 2 == 0 {
                    Color32::from_rgb(21, 30, 39)
                } else {
                    Color32::from_rgb(24, 34, 44)
                },
            );
            let head = Rect::from_min_max(row_rect.min, pos2(timeline_x, row_rect.max.y));
            painter.rect_filled(
                head,
                0.0,
                if self.selected_track == Some(track.id) {
                    Color32::from_rgb(39, 62, 67)
                } else {
                    PANEL
                },
            );
            painter.text(
                head.left_top() + vec2(16.0, 22.0),
                Align2::LEFT_CENTER,
                &track.name,
                FontId::proportional(15.0),
                TEXT,
            );
            painter.text(
                head.left_top() + vec2(16.0, 52.0),
                Align2::LEFT_CENTER,
                format!("A{}  ·  {} clips", row + 1, track.clips.len()),
                FontId::monospace(10.0),
                MUTED,
            );
            let head_response =
                ui.interact(head, ui.id().with(("track", track.id)), Sense::click());
            if head_response.clicked() {
                self.selected_track = Some(track.id);
                self.selected_clip = None;
            }
            painter.line_segment(
                [row_rect.left_bottom(), row_rect.right_bottom()],
                Stroke::new(1.0_f32, GRID),
            );
            for clip in &track.clips {
                let x0 = timeline_x
                    + ((clip.start as f64 / RATE - start_sec) * self.view.target_pps) as f32;
                let x1 = x0 + (clip.len as f64 / RATE * self.view.target_pps) as f32;
                let original = Rect::from_min_max(
                    pos2(x0, top + 9.0),
                    pos2(x1.max(x0 + 3.0), top + ROW_H - 10.0),
                );
                if original.max.x < timeline_x || original.min.x > canvas.max.x {
                    continue;
                }
                let response = ui.interact(
                    original,
                    ui.id().with(("clip", clip.id)),
                    Sense::click_and_drag(),
                );
                if response.clicked() {
                    actions.push(Action::Select(track.id, clip.id));
                }
                let dx = if response.dragged() || response.drag_stopped() {
                    response.drag_delta().x
                } else {
                    0.0
                };
                let body = original.translate(vec2(dx, 0.0));
                painter.rect_filled(
                    body,
                    5.0,
                    if self.selected_clip == Some(clip.id) {
                        Color32::from_rgb(54, 120, 126)
                    } else {
                        CLIP
                    },
                );
                painter.rect_stroke(
                    body,
                    5.0,
                    Stroke::new(
                        if self.selected_clip == Some(clip.id) {
                            2.0_f32
                        } else {
                            1.0_f32
                        },
                        ACCENT,
                    ),
                );
                painter.text(
                    body.left_top() + vec2(9.0, 14.0),
                    Align2::LEFT_CENTER,
                    format!("{} · #{:02}", track.name, clip.id),
                    FontId::proportional(11.0),
                    TEXT,
                );
                self.draw_waveform(&painter, body, clip);
                if clip.fade_in > 0 {
                    let fx = (clip.fade_in as f64 / RATE * self.view.target_pps) as f32;
                    painter.line_segment(
                        [body.left_top(), body.left_top() + vec2(fx, body.height())],
                        Stroke::new(1.0_f32, ACCENT),
                    );
                }
                if clip.fade_out > 0 {
                    let fx = (clip.fade_out as f64 / RATE * self.view.target_pps) as f32;
                    painter.line_segment(
                        [
                            body.right_bottom() - vec2(fx, body.height()),
                            body.right_top(),
                        ],
                        Stroke::new(1.0_f32, ACCENT),
                    );
                }
                if response.drag_stopped() {
                    let shift = (dx as f64 / self.view.target_pps * RATE).round() as i64;
                    let new_start = (clip.start as i64 + shift).max(0) as u64;
                    if new_start != clip.start {
                        actions.push(Action::Move(clip.id, new_start));
                    }
                }
                response.on_hover_text(format!(
                    "Clip #{} · {:.2}s · drag to move",
                    clip.id,
                    clip.len as f64 / RATE
                ));
            }
        }
        let play_x = timeline_x
            + ((self.playhead_frame as f64 / RATE - start_sec) * self.view.target_pps) as f32;
        if play_x >= timeline_x && play_x <= canvas.max.x {
            painter.line_segment(
                [pos2(play_x, canvas.min.y), pos2(play_x, canvas.max.y)],
                Stroke::new(2.0_f32, Color32::from_rgb(245, 204, 105)),
            );
        }
        if tracks.is_empty() {
            painter.text(
                pos2(timeline_x + 30.0, canvas.min.y + RULER_H + 45.0),
                Align2::LEFT_CENTER,
                "Import WAV to add an audio track",
                FontId::proportional(14.0),
                MUTED,
            );
        }
        let maximum = max_scroll(duration, timeline_w, self.view.target_pps);
        self.view.target_scroll = self.view.target_scroll.clamp(0.0, maximum);
    }
    fn draw_waveform(&self, painter: &egui::Painter, body: Rect, clip: &Clip) {
        let Some(bins) = self.peaks.get(&clip.media_id) else {
            return;
        };
        if bins.is_empty() || body.width() < 2.0 {
            return;
        }
        let area = Rect::from_min_max(
            body.left_top() + vec2(3.0, 26.0),
            body.right_bottom() - vec2(3.0, 5.0),
        );
        let visible = painter.clip_rect().intersect(area);
        if visible.width() <= 0.0 {
            return;
        }
        let gain =
            waveform_display_gain(*self.source_peaks.get(&clip.media_id).unwrap_or(&1.0), 0.0);
        for px in visible.min.x.floor() as i32..visible.max.x.ceil() as i32 {
            let fraction = ((px as f32 - body.min.x) / body.width()).clamp(0.0, 1.0);
            let source = clip.source_in + (fraction as f64 * clip.len as f64) as u64;
            let idx = ((source / 256) as usize).min(bins.len() - 1);
            let p = bins[idx];
            let scale = area.height() * 0.44 * gain;
            let y0 = area.center().y - p.max * scale;
            let y1 = area.center().y - p.min * scale;
            painter.line_segment(
                [pos2(px as f32, y0), pos2(px as f32, y1)],
                Stroke::new(1.0_f32, Color32::from_rgb(159, 230, 218)),
            );
        }
    }
    fn save_capture(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.capture_path.clone() {
            let image = ctx.input(|i| {
                i.events.iter().find_map(|event| match event {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let bytes: Vec<u8> = image
                    .pixels
                    .iter()
                    .flat_map(|pixel| pixel.to_array())
                    .collect();
                let result = (|| -> Result<(), String> {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
                    let mut encoder =
                        png::Encoder::new(file, image.size[0] as u32, image.size[1] as u32);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(png::BitDepth::Eight);
                    encoder
                        .write_header()
                        .map_err(|e| e.to_string())?
                        .write_image_data(&bytes)
                        .map_err(|e| e.to_string())?;
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        self.status = format!("Captured {}", path.display());
                        println!("Captured {}", path.display());
                    }
                    Err(e) => eprintln!("Screenshot failed: {e}"),
                }
                self.capture_path = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if !self.capture_requested && self.frame_count > 12 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                self.capture_requested = true;
            }
        }
    }
}

impl eframe::App for AudioStrikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_count += 1;
        if self.frame_count == 1 {
            install_visuals(ctx);
        }
        if let Some(player) = &self.player {
            self.playhead_frame = player.frame() as u64;
            if player.failed() {
                self.status = "Audio device stopped; project remains saved".into();
                self.player = None;
            } else if player.finished() {
                self.player = None;
                self.status = "Playback finished".into();
            }
        }
        self.save_capture(ctx);
        let mut actions = Vec::new();
        let (track, clip) = self.selected();
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(16.0, 11.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("AUDIOSTRIKE")
                            .size(19.0)
                            .strong()
                            .color(ACCENT),
                    );
                    ui.add_space(15.0);
                    if ui
                        .add(button("New"))
                        .on_hover_text("Create a portable .audiostrike project")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("Untitled.audiostrike")
                            .save_file()
                        {
                            actions.push(Action::New(path));
                        }
                    }
                    if ui
                        .add(button("Open"))
                        .on_hover_text("Choose an AudioStrike project folder")
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            actions.push(Action::Open(path));
                        }
                    }
                    if ui
                        .add_enabled(self.project.is_some(), button("Import WAV"))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("PCM WAV", &["wav"])
                            .pick_file()
                        {
                            actions.push(Action::Import(path));
                        }
                    }
                    if ui
                        .add_enabled(self.project.is_some(), button("Bounce WAV"))
                        .clicked()
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("24-bit WAV", &["wav"])
                            .set_file_name("mix.wav")
                            .save_file()
                        {
                            actions.push(Action::Bounce(path));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(
                                self.project
                                    .as_ref()
                                    .map(|p| p.root().display().to_string())
                                    .unwrap_or_else(|| "No project".into()),
                            )
                            .small()
                            .color(MUTED),
                        );
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.project.is_some() && self.player.is_none(),
                            accent_button("Play"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Play);
                    }
                    if ui
                        .add_enabled(self.player.is_some(), button("Stop"))
                        .clicked()
                    {
                        actions.push(Action::Stop);
                    }
                    ui.label(
                        egui::RichText::new(format_time(self.playhead_frame as f64 / RATE))
                            .monospace()
                            .size(18.0)
                            .color(TEXT),
                    );
                    ui.separator();
                    if ui
                        .add_enabled(self.project.is_some(), button("Undo"))
                        .clicked()
                    {
                        actions.push(Action::Undo);
                    }
                    if ui
                        .add_enabled(self.project.is_some(), button("Redo"))
                        .clicked()
                    {
                        actions.push(Action::Redo);
                    }
                    ui.separator();
                    if ui.add(button("-")).on_hover_text("Zoom out").clicked() {
                        zoom_about(
                            &mut self.view,
                            0.7,
                            self.playhead_frame as f64 / RATE,
                            800.0,
                        );
                    }
                    ui.label(format!("{:.0} px/s", self.view.target_pps));
                    if ui.add(button("+")).on_hover_text("Zoom in").clicked() {
                        zoom_about(
                            &mut self.view,
                            1.4,
                            self.playhead_frame as f64 / RATE,
                            800.0,
                        );
                    }
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("48 kHz · 24-bit export")
                            .small()
                            .color(MUTED),
                    );
                });
            });
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(270.0)
            .min_width(230.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::same(15.0)),
            )
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.heading("Inspector");
                ui.label(
                    egui::RichText::new("Precise controls · saved with the session")
                        .small()
                        .color(MUTED),
                );
                ui.separator();
                if self.project.is_none() {
                    ui.label("New project tempo");
                    ui.add(
                        egui::DragValue::new(&mut self.new_tempo)
                            .range(40..=240)
                            .suffix(" BPM"),
                    );
                } else {
                    ui.horizontal(|ui| {
                        ui.label("New track");
                        ui.text_edit_singleline(&mut self.track_name);
                    });
                    if ui.add(button("+ Add empty track")).clicked() {
                        actions.push(Action::AddTrack(self.track_name.clone()));
                    }
                    ui.add_space(12.0);
                    if let Some(track) = &track {
                        ui.label(
                            egui::RichText::new(&track.name)
                                .size(17.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(format!("Track #{}", track.id));
                        ui.horizontal(|ui| {
                            if ui.selectable_label(track.mute, "Mute").clicked() {
                                actions.push(Action::Mute(track.id, !track.mute));
                            }
                            if ui.selectable_label(track.solo, "Solo").clicked() {
                                actions.push(Action::Solo(track.id, !track.solo));
                            }
                        });
                        ui.add_space(8.0);
                        let db = 20.0 * (track.gain as f64).max(1e-6).log10();
                        let mut fader = db_to_pos(db);
                        ui.label(format!("Volume   {db:.1} dB"));
                        if ui
                            .add(egui::Slider::new(&mut fader, 0.0..=1.0).show_value(false))
                            .changed()
                        {
                            actions.push(Action::Gain(track.id, pos_to_db(fader)));
                        }
                        let mut pan = track.pan;
                        ui.label(format!("Pan   {pan:+.2}"));
                        if ui
                            .add(egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false))
                            .changed()
                        {
                            actions.push(Action::Pan(track.id, pan));
                        }
                    } else {
                        ui.label("Select a track to adjust its mixer strip.");
                    }
                    ui.separator();
                    if let Some(clip) = &clip {
                        ui.label(
                            egui::RichText::new(format!("Clip #{}", clip.id))
                                .size(16.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(format!(
                            "Source #{} · {:.2} s",
                            clip.media_id,
                            clip.len as f64 / RATE
                        ));
                        let mut start = clip.start as f64 / RATE;
                        ui.horizontal(|ui| {
                            ui.label("Start");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut start)
                                        .speed(0.05)
                                        .range(0.0..=3600.0)
                                        .suffix(" s"),
                                )
                                .changed()
                            {
                                actions.push(Action::Move(clip.id, (start * RATE).round() as u64));
                            }
                        });
                        let mut fade_in = clip.fade_in as f64 / RATE;
                        let mut fade_out = clip.fade_out as f64 / RATE;
                        ui.horizontal(|ui| {
                            ui.label("Fade in");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut fade_in)
                                        .speed(0.02)
                                        .range(0.0..=clip.len as f64 / RATE)
                                        .suffix(" s"),
                                )
                                .changed()
                            {
                                actions.push(Action::Fades(
                                    clip.id,
                                    (fade_in * RATE) as u64,
                                    clip.fade_out,
                                ));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Fade out");
                            if ui
                                .add(
                                    egui::DragValue::new(&mut fade_out)
                                        .speed(0.02)
                                        .range(0.0..=clip.len as f64 / RATE)
                                        .suffix(" s"),
                                )
                                .changed()
                            {
                                actions.push(Action::Fades(
                                    clip.id,
                                    clip.fade_in,
                                    (fade_out * RATE) as u64,
                                ));
                            }
                        });
                        let inside = self.playhead_frame > clip.start
                            && self.playhead_frame < clip.start + clip.len;
                        if ui
                            .add_enabled(inside, button("Split at playhead"))
                            .clicked()
                        {
                            actions.push(Action::Split(clip.id, self.playhead_frame));
                        }
                    } else {
                        ui.label("Select a clip for move, fades, and split.");
                    }
                }
            });
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::none()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(15.0, 7.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&self.status).color(TEXT));
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new("LOCAL  ·  NO ACCOUNT")
                                .small()
                                .color(ACCENT),
                        );
                    });
                });
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("ARRANGEMENT")
                            .small()
                            .strong()
                            .color(MUTED),
                    );
                    ui.add_space(12.0);
                    if let Some(project) = &self.project {
                        ui.label(format!(
                            "{} tracks · {} stems",
                            project.session().tracks.len(),
                            project.session().media.len()
                        ));
                    }
                });
                ui.add_space(8.0);
                egui::ScrollArea::vertical().show(ui, |ui| self.timeline(ui, &mut actions));
                if self.project.is_some() {
                    let duration = self
                        .project
                        .as_ref()
                        .unwrap()
                        .session()
                        .tracks
                        .iter()
                        .flat_map(|t| &t.clips)
                        .map(|c| c.start + c.len)
                        .max()
                        .unwrap_or(0) as f64
                        / RATE;
                    let max = max_scroll(
                        duration,
                        ui.available_width() - HEADER_W,
                        self.view.target_pps,
                    );
                    ui.add(
                        egui::Slider::new(&mut self.view.target_scroll, 0.0..=max.max(0.001))
                            .text("Timeline scroll")
                            .show_value(false),
                    );
                }
            });
        let save_shortcut = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
        if save_shortcut {
            self.save_after("Project saved");
        }
        if !ctx.wants_keyboard_input() {
            let undo = ctx.input(|i| {
                i.modifiers.command && !i.modifiers.shift && i.key_pressed(egui::Key::Z)
            });
            let redo = ctx.input(|i| {
                i.modifiers.command
                    && (i.key_pressed(egui::Key::Y)
                        || (i.modifiers.shift && i.key_pressed(egui::Key::Z)))
            });
            let toggle_play = ctx.input(|i| i.key_pressed(egui::Key::Space));
            if self.project.is_some() {
                if undo {
                    actions.push(Action::Undo);
                } else if redo {
                    actions.push(Action::Redo);
                }
                if toggle_play {
                    actions.push(if self.player.is_some() {
                        Action::Stop
                    } else {
                        Action::Play
                    });
                }
            }
        }
        for action in actions {
            self.action(action);
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

fn format_time(seconds: f64) -> String {
    let s = seconds.max(0.0);
    format!("{:02}:{:05.2}", (s / 60.0).floor() as u64, s % 60.0)
}
fn compute_peaks(wav: &WavData) -> (Vec<Peak>, f32) {
    let mut bins = Vec::new();
    let mut source_peak = 0.0_f32;
    let channels = wav.channels as usize;
    for chunk in wav.samples.chunks(256 * channels) {
        let min = chunk.iter().copied().fold(0.0_f32, f32::min);
        let max = chunk.iter().copied().fold(0.0_f32, f32::max);
        source_peak = source_peak.max(min.abs()).max(max.abs());
        bins.push(Peak { min, max });
    }
    (bins, source_peak)
}
fn create_demo(root: &Path) -> Result<Project, String> {
    if root.exists() {
        return Project::open(root).map_err(|e| e.to_string());
    }
    let mut project = Project::create(root, 112).map_err(|e| e.to_string())?;
    for (index, (name, hz, rate)) in [
        ("Lead Voice", 220.0, 48_000_u32),
        ("Keys", 329.63, 44_100_u32),
    ]
    .into_iter()
    .enumerate()
    {
        let wav = root.with_extension(format!("stem-{}.wav", index + 1));
        let samples = (0..rate * 8)
            .map(|i| {
                let time = i as f64 / rate as f64;
                let shape = (time * 0.7).sin().abs() * 0.65 + 0.25;
                (0.16 * shape * (std::f64::consts::TAU * hz * time).sin()) as f32
            })
            .collect();
        write_wav_float(
            &wav,
            &WavData {
                sample_rate: rate,
                channels: 1,
                samples,
            },
        )
        .map_err(|e| e.to_string())?;
        let asset = project.import_wav(&wav).map_err(|e| e.to_string())?;
        let track = project.add_track(name).map_err(|e| e.to_string())?;
        project
            .place_clip(track, asset, if index == 0 { 0 } else { 48_000 })
            .map_err(|e| e.to_string())?;
        project
            .set_track_pan(track, if index == 0 { -0.22 } else { 0.25 })
            .map_err(|e| e.to_string())?;
    }
    project.save().map_err(|e| e.to_string())?;
    Ok(project)
}
fn smoke_audio(path: &Path) -> Result<String, String> {
    let project = Project::open(path).map_err(|e| e.to_string())?;
    let mixer = StreamingMixer::open(&project).map_err(|e| e.to_string())?;
    let player = AudioPlayer::start_streaming(mixer, 0)?;
    std::thread::sleep(Duration::from_millis(500));
    if player.failed() || player.frame() == 0 {
        return Err("audio callback did not advance for 500 ms".into());
    }
    Ok(format!(
        "Audio callback advanced {} frames on {}",
        player.frame(),
        player.device_name
    ))
}
fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--smoke-audio") {
        let result = args
            .get(i + 1)
            .ok_or("--smoke-audio needs a .audiostrike folder".to_string())
            .and_then(|path| smoke_audio(Path::new(path)));
        match result {
            Ok(message) => println!("{message}"),
            Err(error) => {
                eprintln!("Audio smoke failed: {error}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--probe-audio") {
        match probe_output() {
            Ok(message) => println!("{message}"),
            Err(error) => eprintln!("Audio output unavailable: {error}"),
        }
        return Ok(());
    }
    let demo_at = args.iter().position(|a| a == "--demo");
    let project_at = args.iter().position(|a| a == "--project");
    let capture_at = args.iter().position(|a| a == "--capture");
    let project = if let Some(i) = demo_at {
        args.get(i + 1)
            .and_then(|path| match create_demo(Path::new(path)) {
                Ok(project) => Some(project),
                Err(e) => {
                    eprintln!("Demo: {e}");
                    None
                }
            })
    } else if let Some(i) = project_at {
        args.get(i + 1)
            .and_then(|path| Project::open(Path::new(path)).ok())
    } else {
        None
    };
    let capture = capture_at.and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1320.0, 790.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "AudioStrike",
        options,
        Box::new(move |_cc| Ok(Box::new(AudioStrikeApp::new(project, capture)))),
    )
}

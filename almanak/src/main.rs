//! Almanak: PhotoCraft, VectorCraft and DesignCraft as the Photo, Vector and Layout personas of
//! one window, switched from the persona bar.
//!
//! Usage: `almanak [options] [files…]`. Each file opens in the persona that edits its kind (`.psd`,
//! `.png` → Photo; `.svg`, `.vectorcraft` → Vector; `.designcraft`, `.idml` → Layout).
//!
//! `--control <port>` starts a JSON-lines control channel per persona, each speaking that app's
//! own protocol: Photo on `port`, Vector on `port + 1`, Layout on `port + 2`. All three ask for the
//! same bearer token first (`{"method":"auth","params":{"token":…}}`; `--control-token-file
//! <path>`, else one is generated and printed). `--automation-read-root` / `--automation-write-root`
//! confine the files Photo and Vector agents may read and write (Photo: no file access without
//! them); Layout's channel is not confined. A request on a persona's channel brings that persona to
//! the front, so an agent sees what it drives. `almanak.switch {"persona": "photo"|"vector"|"layout"}`
//! on any channel does what the persona bar does (StudioLink included) and replies with its status.
//!
//! The three apps share one egui context: only the active persona runs its frame, and switching
//! re-installs its fonts and theme (`reinstall_style`), which the others replaced.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod studio;

/// PhotoCraft's control server (token auth, request and reply limits) serves all three personas.
#[path = "../../photocraft/apps/photocraft/src/control_server.rs"]
mod control_server;

use designcraft_ui_egui::DesignApp;
use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, PhotocraftApp};
use vectorcraft_engine::file_access;
use vectorcraft_ui_egui::VectorcraftApp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Persona {
    Photo,
    Vector,
    Layout,
}

impl Persona {
    const ALL: [Persona; 3] = [Persona::Photo, Persona::Vector, Persona::Layout];

    /// The persona that edits files like `path`; `None` when it's unknown (Photo opens most images).
    fn for_path(path: &str) -> Option<Persona> {
        let ext = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "vectorcraft" | "drawcraft" | "svg" | "svgz" | "ai" | "eps" | "pdf" | "afdesign" => Persona::Vector,
            "designcraft" | "idml" | "afpub" => Persona::Layout,
            _ => Persona::Photo,
        })
    }
}

/// Control requests waiting for each persona (indexed by `Persona as usize`): the persona's own
/// frame takes them, and it only runs while that persona is in front.
type Pending = Arc<[AtomicBool; 3]>;

/// An `almanak.switch` request from the control channel: the persona, and where to reply.
type SwitchRequest = Arc<std::sync::Mutex<Option<(Persona, std::sync::mpsc::Sender<serde_json::Value>)>>>;

struct Almanak {
    active: Persona,
    pending: Pending,
    switch_request: SwitchRequest,
    photo: PhotocraftApp,
    vector: VectorcraftApp,
    layout: DesignApp,
    /// Placed graphics being edited in Photo or Vector.
    studio: studio::Studio,
    /// The last StudioLink message, shown on the persona bar.
    status: String,
    /// Quitting is waiting on Layout's Save Changes question about this document (uid).
    quitting_layout: Option<u64>,
}

/// The persona bar's height, in points.
const BAR_HEIGHT: f32 = 26.0;

impl Almanak {
    /// Bring `to` to the front. `user`: the persona bar, so StudioLink opens the selected graphic
    /// in `to`; a control request or quitting only switches (and still writes edits back).
    fn switch(&mut self, to: Persona, ctx: &egui::Context, user: bool) {
        if to == self.active {
            return;
        }
        let result = match (self.active, to) {
            (Persona::Layout, _) if user => self.studio.edit_selected(to, &self.layout, &mut self.photo, &mut self.vector),
            (_, Persona::Layout) => self.studio.write_back(&mut self.layout, &mut self.photo, &mut self.vector),
            _ => Ok(None),
        };
        match result {
            Ok(Some(msg)) => self.status = msg,
            Ok(None) => {}
            Err(e) => {
                log::error!("StudioLink: {e}");
                self.status = format!("Couldn't link the graphic: {e}");
            }
        }
        self.active = to;
        match to {
            Persona::Photo => self.photo.reinstall_style(),
            Persona::Vector => self.vector.reinstall_style(),
            Persona::Layout => self.layout.reinstall_style(),
        }
        ctx.request_repaint();
    }

    fn open(&mut self, path: &str) {
        let result = match Persona::for_path(path).unwrap_or(Persona::Photo) {
            Persona::Photo => {
                self.photo.open_paths(&[path.to_string()]);
                Ok(())
            }
            Persona::Vector => file_access::unconfined(|| vectorcraft_ui_egui::io::open_reporting(&mut self.vector, path).map(drop)),
            Persona::Layout => self.layout.open_file("file.open", serde_json::json!({ "path": path })).map(drop),
        };
        if let Err(e) = result {
            log::error!("{path}: {e}");
        }
    }

    /// The window is asked to close: each persona with unsaved documents asks about them in turn,
    /// in front (Photo and Vector with their own Save Changes questions, which close again when
    /// answered; Layout one document at a time). Returns whether the close may go ahead.
    fn may_close(&mut self, ctx: &egui::Context) -> bool {
        // Edited graphics go into the layout first: then only the layout asks about them.
        match self.studio.write_back(&mut self.layout, &mut self.photo, &mut self.vector) {
            Ok(Some(msg)) => self.status = msg,
            Ok(None) => {}
            Err(e) => self.status = format!("Couldn't update the layout: {e}"),
        }
        let photo_unsaved = !self.photo.close_allowed() && self.photo.session.documents().iter().any(|d| d.is_dirty());
        if photo_unsaved {
            self.switch(Persona::Photo, ctx, false);
            return true; // Photo's frame holds the close back and asks.
        }
        if self.vector.session.documents().iter().any(|d| d.is_dirty()) {
            self.switch(Persona::Vector, ctx, false);
            return true; // Vector's frame holds the close back and asks.
        }
        if let Some(d) = self.layout.session.documents().iter().position(|d| d.is_dirty()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.switch(Persona::Layout, ctx, false);
            self.quitting_layout = self.layout.session.documents().get(d).map(|d| d.uid);
            designcraft_ui_egui::menus::close_document(&mut self.layout, Some(d));
            return false;
        }
        true
    }

    /// While quitting waits on Layout: once its question is answered, ask about the next unsaved
    /// document, or close. Cancel (the document still unsaved) stops quitting.
    fn continue_quit(&mut self, ctx: &egui::Context) {
        let Some(uid) = self.quitting_layout else { return };
        if self.layout.ui.dialog.is_some() {
            return;
        }
        self.quitting_layout = None;
        let cancelled = self.layout.session.documents().iter().any(|d| d.uid == uid && d.is_dirty());
        if !cancelled {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn persona_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("almanak_personas").exact_size(BAR_HEIGHT).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.strong("Almanak");
                ui.separator();
                for p in Persona::ALL {
                    if ui.selectable_label(self.active == p, format!("{p:?}")).clicked() {
                        self.switch(p, ui.ctx(), true);
                    }
                }
                if !self.status.is_empty() {
                    ui.separator();
                    ui.weak(&self.status);
                }
            });
        });
    }
}

impl eframe::App for Almanak {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Control requests for a persona in the back: bring it forward so its frame takes them.
        let waiting = |p: Persona| self.pending[p as usize].load(Ordering::Acquire);
        if !waiting(self.active)
            && let Some(p) = Persona::ALL.into_iter().find(|p| waiting(*p))
        {
            self.switch(p, ctx, false);
        }
        let asked = self.switch_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some((p, reply)) = asked {
            self.status.clear();
            self.switch(p, ctx, true);
            let _ = reply.send(serde_json::json!({"ok": true, "result": {"persona": format!("{:?}", self.active).to_lowercase(), "status": self.status}}));
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.may_close(ctx) {
            return;
        }
        self.continue_quit(ctx);
        // Cleared before the frame that takes them: a request arriving later sets it again.
        self.pending[self.active as usize].store(false, Ordering::Release);
        // ponytail: only the active persona runs; background jobs of the others wait until shown.
        match self.active {
            Persona::Photo => eframe::App::logic(&mut self.photo, ctx, frame),
            // Vector confines what agents ask for to the automation roots; the person at the
            // keyboard is not confined (as in VectorCraft's own shell).
            Persona::Vector => file_access::unconfined(|| self.vector.logic(ctx)),
            Persona::Layout => self.layout.logic(ctx),
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        match self.active {
            Persona::Photo => eframe::App::raw_input_hook(&mut self.photo, ctx, raw),
            Persona::Vector => file_access::unconfined(|| self.vector.raw_input_hook(raw)),
            Persona::Layout => self.layout.raw_input_hook(raw),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.persona_bar(ui);
        match self.active {
            Persona::Photo => eframe::App::ui(&mut self.photo, ui, frame),
            Persona::Vector => file_access::unconfined(|| self.vector.ui(ui)),
            Persona::Layout => self.layout.ui(ui),
        }
    }

    fn on_exit(&mut self) {
        // Photo saves its preferences as they change (`Services::save_prefs`).
        save_vector_prefs(&self.vector);
        save_layout_prefs(&self.layout);
    }
}

/// Hand control requests from `rx` to a persona through the returned receiver (as its own request
/// type), marking it as waiting so it comes to the front. `almanak.switch` goes to Almanak.
fn relay<T: Send + 'static>(
    rx: std::sync::mpsc::Receiver<photocraft_ui_egui::ControlRequest>,
    persona: Persona,
    (pending, switch): (Pending, SwitchRequest),
    ctx: egui::Context,
    convert: fn(photocraft_ui_egui::ControlRequest) -> T,
) -> std::sync::mpsc::Receiver<T> {
    let (tx, out) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for request in rx {
            if request.method == "almanak.switch" {
                let to = match request.params.get("persona").and_then(serde_json::Value::as_str) {
                    Some("photo") => Persona::Photo,
                    Some("vector") => Persona::Vector,
                    Some("layout") => Persona::Layout,
                    _ => {
                        let _ = request.reply.send(serde_json::json!({"ok": false, "error": "persona: photo, vector or layout"}));
                        continue;
                    }
                };
                *switch.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((to, request.reply));
                ctx.request_repaint();
                continue;
            }
            if tx.send(convert(request)).is_err() {
                break;
            }
            pending[persona as usize].store(true, Ordering::Release);
            ctx.request_repaint();
        }
    });
    out
}

/// Almanak's settings folder for `persona` (`$XDG_CONFIG_HOME/almanak/<persona>`, else
/// `~/.config/almanak/<persona>`).
fn config_dir(persona: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("almanak").join(persona))
}

/// Write `bytes` to `name` in `persona`'s settings folder; best effort (the old file stays on failure).
fn write_setting(persona: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = config_dir(persona).ok_or("no settings folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    photocraft_format::atomic_write(&dir.join(name), bytes).map_err(|e| e.to_string())
}

fn read_setting(persona: &str, name: &str) -> Option<Vec<u8>> {
    std::fs::read(config_dir(persona)?.join(name)).ok()
}

fn load_vector_prefs(app: &mut VectorcraftApp) {
    if let Some(ui) = read_setting("vector", "ui.json").and_then(|b| serde_json::from_slice::<vectorcraft_ui_egui::UiState>(&b).ok()) {
        app.ui = ui.sanitized();
    }
    vectorcraft_ui_egui::prefs_dialog::restore(app);
}

fn save_vector_prefs(app: &VectorcraftApp) {
    let mut ui = app.ui.clone();
    ui.engine_prefs = app.session.prefs.to_json();
    if let Ok(bytes) = serde_json::to_vec_pretty(&ui) {
        let _ = write_setting("vector", "ui.json", &bytes);
    }
}

fn load_layout_prefs(app: &mut DesignApp) {
    if let Some(ui) = read_setting("layout", "ui.json").and_then(|b| designcraft_ui_egui::UiState::from_preferences_json(&b).ok()) {
        app.ui = ui;
    }
    if let Some(prefs) = read_setting("layout", "prefs.json").and_then(|b| serde_json::from_slice::<designcraft_engine::Prefs>(&b).ok()) {
        app.session.prefs = prefs;
    }
}

fn save_layout_prefs(app: &DesignApp) {
    if let Ok(bytes) = serde_json::to_vec_pretty(&app.ui) {
        let _ = write_setting("layout", "ui.json", &bytes);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&app.session.prefs) {
        let _ = write_setting("layout", "prefs.json", &bytes);
    }
}

/// PhotoCraft's services; `automation` (the control channel's folders) lets agents read and write
/// files there.
fn photo_services(automation: Option<photocraft_automation::AuthorizedWorkspace>) -> photocraft_ui_egui::Services {
    use photocraft_ui_egui::{ExportSettings, jobs_ui::SaveCtl};
    fn export_options(s: &ExportSettings) -> photocraft_io::ExportOptions {
        let mut o = photocraft_io::ExportOptions::default();
        if let Some(q) = s.jpeg_quality {
            o.encode.jpeg_quality = q;
        }
        o.encode.webp_lossless = s.webp_lossless;
        if let Some(q) = s.webp_quality {
            o.encode.webp_quality = q;
        }
        o.tiff_layers = s.tiff_layers;
        o.xmp = if s.xmp_all { photocraft_io::XmpEmbed::All } else { photocraft_io::XmpEmbed::None };
        o.tga_bits = s.tga_bits;
        o
    }
    let save_through = |write: Box<dyn Fn(&str, &[u8]) -> Result<(), String> + Send + Sync>| -> photocraft_ui_egui::SaveFileFn {
        std::sync::Arc::new(move |doc: &photocraft_doc::Document, path: &str, s: &ExportSettings, ctl: &SaveCtl| {
            ctl.progress(0.0, "Encoding");
            let r = photocraft_io::export(doc, path, &export_options(s)).map_err(|e| e.to_string())?;
            ctl.commit()?;
            ctl.progress(0.0, "Writing");
            write(path, &r.bytes)?;
            Ok(r.warnings)
        })
    };
    let automation_read = automation.clone().map(|w| {
        Box::new(move |path: &str| {
            let bytes = w.read(path).map_err(|e| e.to_string())?;
            let name = Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path).to_string();
            Ok((name, bytes))
        }) as photocraft_ui_egui::AutomationReadFn
    });
    let automation_write = automation.clone().map(|w| {
        Box::new(move |path: &str, bytes: &[u8]| w.write(path, bytes).map_err(|e| e.to_string())) as photocraft_ui_egui::AutomationWriteFn
    });
    let automation_save = automation.clone().map(|w| save_through(Box::new(move |p, b| w.write(p, b).map_err(|e| e.to_string()))));
    let step: fn(&str, &serde_json::Value) -> photocraft_engine::Result<()> = photocraft_automation::workspace::authorize_desktop_engine_step;
    let automation_command = automation.as_ref().map(|_| {
        Box::new(|id: &str, params: &serde_json::Value| {
            photocraft_automation::workspace::authorize_desktop_engine_command(id, params).map_err(|e| e.to_string())
        }) as photocraft_ui_egui::AutomationCommandFn
    });
    // ponytail: the essentials of PhotoCraft's native services (open, save, export, dialogs);
    // clipboard images, recovery, presets and the rest come with the shared-document work.
    photocraft_ui_egui::Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], depth: usize| {
            photocraft_io::import_with_svg_group_depth(name, bytes, depth).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &photocraft_doc::Document, path: &str, s: &ExportSettings| {
            photocraft_io::export(doc, path, &export_options(s)).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        save_file: Some(save_through(Box::new(|p, b| photocraft_format::atomic_write(Path::new(p), b).map_err(|e| e.to_string())))),
        automation_authorize: automation.is_some().then_some(step),
        automation_read,
        automation_write,
        automation_save,
        automation_command,
        write: Some(Box::new(|path: &str, bytes: &[u8]| photocraft_format::atomic_write(Path::new(path), bytes).map_err(|e| e.to_string()))),
        // Dialogs block, so they run on a thread of their own and answer through `reply`.
        file_dialog: Some(Box::new(|request, _parent, reply| {
            std::thread::spawn(move || {
                let answer = match request {
                    FileDialogRequest::Open { multiple, initial_dir, extensions } => {
                        let mut d = rfd::FileDialog::new();
                        if let Some(dir) = initial_dir {
                            d = d.set_directory(dir);
                        }
                        if let Some(exts) = extensions {
                            d = d.add_filter("Supported Files", &exts);
                        }
                        let picked = if multiple { d.pick_files() } else { d.pick_file().map(|p| vec![p]) };
                        picked.map(|ps| FileDialogAnswer::Paths(ps.iter().map(|p| p.to_string_lossy().into_owned()).collect()))
                    }
                    FileDialogRequest::Save { suggested } => {
                        let p = Path::new(&suggested);
                        let mut d = rfd::FileDialog::new();
                        if let Some(name) = p.file_name() {
                            d = d.set_file_name(name.to_string_lossy());
                        }
                        if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                            d = d.set_directory(dir);
                        }
                        d.save_file().map(|p| FileDialogAnswer::SaveTo(p.to_string_lossy().into_owned()))
                    }
                };
                reply.send(answer);
            });
        })),
        encode_png: Some(Box::new(|w, h, rgba| {
            use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
            let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).map_err(|e| e.to_string())?;
            photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())
        })),
        // Only the web: the apps' own Help links. (An https:// URL can't read as an option, and
        // xdg-open takes no `--`.)
        open_url: Some(Box::new(|url: &str| {
            if !url.starts_with("https://") {
                return Err(format!("not a web address: {url}"));
            }
            std::process::Command::new("xdg-open").arg(url).spawn().map(drop).map_err(|e| e.to_string())
        })),
        load_prefs: Some(Box::new(|| read_setting("photo", "preferences.json").and_then(|b| String::from_utf8(b).ok()))),
        save_prefs: Some(Box::new(|text: &str| write_setting("photo", "preferences.json", text.as_bytes()))),
        ..Default::default()
    }
}

fn vector_services() -> vectorcraft_ui_egui::Services {
    use vectorcraft_ui_egui::FilePick;
    let dialog = |pick: &FilePick| {
        let mut d = rfd::FileDialog::new().set_file_name(&pick.name);
        if let Some(dir) = &pick.folder {
            d = d.set_directory(dir);
        }
        for (label, exts) in &pick.filters {
            d = d.add_filter(*label, exts);
        }
        d
    };
    vectorcraft_ui_egui::Services {
        pick_open: Some(Box::new(move |pick: &FilePick| dialog(pick).pick_file().map(|p| p.to_string_lossy().into_owned()))),
        pick_save: Some(Box::new(move |pick: &FilePick| dialog(pick).save_file().map(|p| p.to_string_lossy().into_owned()))),
        read: Some(Box::new(|p: &str| std::fs::read(p).map_err(|e| e.to_string()))),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    }
}

fn layout_services() -> designcraft_ui_egui::Services {
    designcraft_ui_egui::Services {
        pick_open: Some(Box::new(|_purpose: &str| rfd::FileDialog::new().pick_file().map(|p| p.to_string_lossy().into_owned()))),
        pick_save: Some(Box::new(|name: &str| rfd::FileDialog::new().set_file_name(name).save_file().map(|p| p.to_string_lossy().into_owned()))),
        read: Some(Box::new(|p: &str| std::fs::read(p).map_err(|e| e.to_string()))),
        write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
        ..Default::default()
    }
}

/// Report a usage error and stop.
fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("almanak: {msg}");
    std::process::exit(2)
}

struct Args {
    files: Vec<String>,
    control_port: Option<u16>,
    token_file: Option<PathBuf>,
    read_root: Option<PathBuf>,
    write_root: Option<PathBuf>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args { files: Vec::new(), control_port: None, token_file: None, read_root: None, write_root: None };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            // port + 2 must fit; below 1024 needs privileges (and 0 picks a random port).
            "--control" => {
                let v = value()?;
                a.control_port = Some(v.parse::<u16>().ok().filter(|p| (1024..=u16::MAX - 2).contains(p)).ok_or(format!("--control: {v:?} is not a port from 1024 to 65533"))?);
            }
            "--control-token-file" => a.token_file = Some(value()?.into()),
            "--automation-read-root" => a.read_root = Some(value()?.into()),
            "--automation-write-root" => a.write_root = Some(value()?.into()),
            "--" => a.files.extend(args.by_ref()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => a.files.push(arg),
        }
    }
    Ok(a)
}

fn main() -> eframe::Result {
    let Args { files, control_port, token_file, read_root, write_root } = parse_args(std::env::args().skip(1)).unwrap_or_else(|e| die(e));
    // The folders agents may read and write through the control channels (none: no file access
    // for Photo, nothing checked for Vector).
    let (photo_roots, vector_roots) = match control_port {
        Some(_) => (
            Some(photocraft_automation::AuthorizedWorkspace::new(read_root.as_deref(), write_root.as_deref()).unwrap_or_else(|e| die(format!("automation roots: {e}")))),
            file_access::AutomationRoots::new(read_root.as_deref(), write_root.as_deref()).unwrap_or_else(|e| die(format!("automation roots: {e}"))),
        ),
        None => (None, None),
    };
    let token = control_port.map(|_| {
        let (supplied, file) = photocraft_automation::security::token_inputs(None, token_file);
        let token = photocraft_automation::security::server_token(supplied.as_deref(), file.as_deref()).unwrap_or_else(|e| die(format!("control token: {e}")));
        match (&file, &supplied) {
            (Some(path), _) => eprintln!("almanak: control token file: {}", path.display()),
            (None, Some(_)) => eprintln!("almanak: using the supplied control token"),
            // Generated for this run and kept nowhere else: the only way for a client to get it.
            (None, None) => eprintln!("almanak: control token: {token}"),
        }
        token
    });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Almanak")
            .with_app_id("almanak")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "Almanak",
        options,
        Box::new(move |cc| {
            let mut photo = PhotocraftApp::new(photocraft_engine::Session::new(), photo_services(photo_roots));
            // Opens finish in the frame they start in, so StudioLink sees the opened document.
            photo.background_jobs = false;
            if let Some(rs) = cc.wgpu_render_state.clone() {
                photo.set_wgpu(rs);
            }
            let mut vector = VectorcraftApp::new(vectorcraft_engine::Session::new(), vector_services());
            load_vector_prefs(&mut vector);
            let mut layout = DesignApp::new(designcraft_engine::Session::new(), layout_services());
            load_layout_prefs(&mut layout);
            let pending: Pending = Arc::new([AtomicBool::new(false), AtomicBool::new(false), AtomicBool::new(false)]);
            let switch_request = SwitchRequest::default();
            let shared = || (pending.clone(), switch_request.clone());
            if let (Some(port), Some(token)) = (control_port, token) {
                let ctx = &cc.egui_ctx;
                let rx = control_server::start(port, token.clone(), ctx.clone());
                photo = photo.with_control(relay(rx, Persona::Photo, shared(), ctx.clone(), |r| r));
                let rx = control_server::start(port + 1, token.clone(), ctx.clone());
                let to_vector = |r: photocraft_ui_egui::ControlRequest| vectorcraft_ui_egui::ControlRequest { method: r.method, params: r.params, reply: r.reply };
                vector = vector.with_control(relay(rx, Persona::Vector, shared(), ctx.clone(), to_vector)).with_automation_roots(vector_roots.clone());
                if let Some(roots) = vector_roots {
                    // Everywhere Vector doesn't say whose work it is (worker threads), the roots hold.
                    file_access::confine_process(roots);
                }
                let rx = control_server::start(port + 2, token, ctx.clone());
                let to_layout = |r: photocraft_ui_egui::ControlRequest| designcraft_ui_egui::ControlRequest { method: r.method, params: r.params, reply: r.reply };
                layout = layout.with_control(relay(rx, Persona::Layout, shared(), ctx.clone(), to_layout));
            }
            let mut app = Almanak {
                active: files.first().and_then(|f| Persona::for_path(f)).unwrap_or(Persona::Photo),
                pending,
                switch_request,
                photo,
                vector,
                layout,
                studio: studio::Studio::default(),
                status: String::new(),
                quitting_layout: None,
            };
            for f in &files {
                app.open(f);
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{Persona, parse_args};

    #[test]
    fn files_open_in_the_persona_that_edits_them() {
        assert_eq!(Persona::for_path("a.PSD"), Some(Persona::Photo));
        assert_eq!(Persona::for_path("logo.svg"), Some(Persona::Vector));
        assert_eq!(Persona::for_path("x/y.vectorcraft"), Some(Persona::Vector));
        assert_eq!(Persona::for_path("mag.designcraft"), Some(Persona::Layout));
        assert_eq!(Persona::for_path("noext"), None);
    }

    #[test]
    fn command_line() {
        let parse = |v: &[&str]| parse_args(v.iter().map(|s| s.to_string()));
        let a = parse(&["--control", "7979", "--control-token-file", "t", "a.psd", "--", "--odd.png"]).unwrap();
        assert_eq!(a.control_port, Some(7979));
        assert_eq!(a.files, ["a.psd", "--odd.png"]);
        assert!(parse(&["--control", "0"]).is_err(), "port 0 would pick a random port for Photo only");
        assert!(parse(&["--control", "65534"]).is_err(), "port + 2 must fit");
        assert!(parse(&["--control"]).is_err());
        assert!(parse(&["--control-token-file"]).is_err());
        assert!(parse(&["--contrl", "7979"]).is_err(), "a mistyped option is not a file");
    }
}

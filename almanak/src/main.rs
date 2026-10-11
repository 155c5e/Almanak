//! Almanak: PhotoCraft, VectorCraft and DesignCraft as the Photo, Vector and Layout personas of
//! one window, switched from the persona bar.
//!
//! Usage: `almanak [files…]`. Each file opens in the persona that edits its kind (`.psd`, `.png`
//! → Photo; `.svg`, `.vectorcraft` → Vector; `.designcraft`, `.idml` → Layout).
//!
//! `--control <port>` starts each app's own JSON-lines control server, unchanged, so each app's
//! MCP bridge and control docs apply: Photo on `port`, Vector on `port + 1`, Layout on `port + 2`.
//! `--automation-read-root` / `--automation-write-root` name the folders agents may read and
//! write (Photo; without them, no file access). Photo asks for a token (`--control-token-file <path>`, or one generated and printed). A request
//! on a persona's port brings that persona to the front, so an agent sees what it drives.
//!
//! The three apps share one egui context: only the active persona runs its frame, and switching
//! re-installs its fonts and theme (`reinstall_style`), which the others replaced.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[path = "../../photocraft/apps/photocraft/src/control_server.rs"]
mod photo_control;
#[path = "../../vectorcraft/apps/vectorcraft/src/control_server.rs"]
mod vector_control;
#[path = "../../designcraft/apps/designcraft/src/control_server.rs"]
mod layout_control;

use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, PhotocraftApp};
use vectorcraft_ui_egui::VectorcraftApp;
use designcraft_ui_egui::DesignApp;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Persona {
    Photo,
    Vector,
    Layout,
}

impl Persona {
    const ALL: [Persona; 3] = [Persona::Photo, Persona::Vector, Persona::Layout];

    fn label(self) -> &'static str {
        match self {
            Persona::Photo => "Photo",
            Persona::Vector => "Vector",
            Persona::Layout => "Layout",
        }
    }

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

/// No persona asked for by the control channel.
const NO_REQUEST: u8 = u8::MAX;

struct Almanak {
    active: Persona,
    /// The persona a control request last arrived for (its index in [`Persona::ALL`]).
    requested: Arc<AtomicU8>,
    photo: PhotocraftApp,
    vector: VectorcraftApp,
    layout: DesignApp,
}

/// The persona bar's height, in points.
const BAR_HEIGHT: f32 = 26.0;

impl Almanak {
    fn switch(&mut self, to: Persona, ctx: &egui::Context) {
        if to == self.active {
            return;
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
            Persona::Vector => vectorcraft_ui_egui::io::open_reporting(&mut self.vector, path).map(drop),
            Persona::Layout => self.layout.open_file("file.open", serde_json::json!({ "path": path })).map(drop),
        };
        if let Err(e) = result {
            log::error!("{path}: {e}");
        }
    }

    fn persona_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("almanak_personas").exact_size(BAR_HEIGHT).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.strong("Almanak");
                ui.separator();
                for p in Persona::ALL {
                    if ui.selectable_label(self.active == p, p.label()).clicked() {
                        self.switch(p, ui.ctx());
                    }
                }
            });
        });
    }
}

impl eframe::App for Almanak {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(&p) = Persona::ALL.get(self.requested.swap(NO_REQUEST, Ordering::Relaxed) as usize) {
            self.switch(p, ctx);
        }
        // ponytail: only the active persona runs; background jobs of the others wait until shown.
        match self.active {
            Persona::Photo => eframe::App::logic(&mut self.photo, ctx, frame),
            Persona::Vector => self.vector.logic(ctx),
            Persona::Layout => self.layout.logic(ctx),
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        match self.active {
            Persona::Photo => eframe::App::raw_input_hook(&mut self.photo, ctx, raw),
            Persona::Vector => self.vector.raw_input_hook(raw),
            Persona::Layout => self.layout.raw_input_hook(raw),
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.persona_bar(ui);
        match self.active {
            Persona::Photo => eframe::App::ui(&mut self.photo, ui, frame),
            Persona::Vector => self.vector.ui(ui),
            Persona::Layout => self.layout.ui(ui),
        }
    }
}

/// Hand requests from `rx` to the app through the returned receiver, asking for `persona` first.
fn relay<T: Send + 'static>(rx: std::sync::mpsc::Receiver<T>, persona: Persona, requested: Arc<AtomicU8>, ctx: egui::Context) -> std::sync::mpsc::Receiver<T> {
    let (tx, out) = std::sync::mpsc::channel();
    let index = Persona::ALL.iter().position(|p| *p == persona).unwrap_or(0) as u8;
    std::thread::spawn(move || {
        for request in rx {
            requested.store(index, Ordering::Relaxed);
            if tx.send(request).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    out
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
        open_url: Some(Box::new(|url: &str| std::process::Command::new("xdg-open").arg(url).spawn().map(drop).map_err(|e| e.to_string()))),
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

fn main() -> eframe::Result {
    let mut files = Vec::new();
    let mut control_port: Option<u16> = None;
    let mut token_file = None;
    let (mut read_root, mut write_root) = (None, None);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--control" => match args.next().and_then(|p| p.parse::<u16>().ok()).filter(|p| *p <= u16::MAX - 2) {
                Some(p) => control_port = Some(p),
                None => {
                    eprintln!("almanak: --control needs a port number");
                    std::process::exit(2);
                }
            },
            "--control-token-file" => token_file = args.next().map(std::path::PathBuf::from),
            "--automation-read-root" => read_root = args.next().map(std::path::PathBuf::from),
            "--automation-write-root" => write_root = args.next().map(std::path::PathBuf::from),
            _ => files.push(a),
        }
    }
    // The folders agents may read and write through the control channel (none: no file access).
    let workspace = match control_port.map(|_| photocraft_automation::AuthorizedWorkspace::new(read_root.as_deref(), write_root.as_deref())) {
        Some(Ok(w)) => Some(w),
        Some(Err(e)) => {
            eprintln!("almanak: automation roots: {e}");
            std::process::exit(2);
        }
        None => None,
    };
    let photo_token = match control_port {
        Some(_) => {
            let (supplied, file) = photocraft_automation::security::token_inputs(None, token_file);
            match photocraft_automation::security::server_token(supplied.as_deref(), file.as_deref()) {
                Ok(token) => {
                    match &file {
                        Some(path) => eprintln!("almanak: Photo control token file: {}", path.display()),
                        None => eprintln!("almanak: Photo control token: {token}"),
                    }
                    Some(token)
                }
                Err(e) => {
                    eprintln!("almanak: control token: {e}");
                    std::process::exit(2);
                }
            }
        }
        None => None,
    };
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
            let mut photo = PhotocraftApp::new(photocraft_engine::Session::new(), photo_services(workspace));
            if let Some(rs) = cc.wgpu_render_state.clone() {
                photo.set_wgpu(rs);
            }
            let mut vector = VectorcraftApp::new(vectorcraft_engine::Session::new(), vector_services());
            let mut layout = DesignApp::new(designcraft_engine::Session::new(), layout_services());
            let requested = Arc::new(AtomicU8::new(NO_REQUEST));
            if let (Some(port), Some(token)) = (control_port, photo_token) {
                let ctx = &cc.egui_ctx;
                let rx = photo_control::start(port, token, ctx.clone());
                photo = photo.with_control(relay(rx, Persona::Photo, requested.clone(), ctx.clone()));
                let rx = vector_control::start(port + 1, ctx.clone());
                vector = vector.with_control(relay(rx, Persona::Vector, requested.clone(), ctx.clone()));
                let rx = layout_control::start(port + 2, ctx.clone());
                layout = layout.with_control(relay(rx, Persona::Layout, requested.clone(), ctx.clone()));
                eprintln!("almanak: control channels: Photo {port}, Vector {}, Layout {}", port + 1, port + 2);
            }
            let mut app = Almanak {
                active: files.first().and_then(|f| Persona::for_path(f)).unwrap_or(Persona::Photo),
                requested,
                photo,
                vector,
                layout,
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
    use super::Persona;

    #[test]
    fn files_open_in_the_persona_that_edits_them() {
        assert_eq!(Persona::for_path("a.PSD"), Some(Persona::Photo));
        assert_eq!(Persona::for_path("logo.svg"), Some(Persona::Vector));
        assert_eq!(Persona::for_path("x/y.vectorcraft"), Some(Persona::Vector));
        assert_eq!(Persona::for_path("mag.designcraft"), Some(Persona::Layout));
        assert_eq!(Persona::for_path("noext"), None);
    }
}

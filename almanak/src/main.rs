//! Almanak: PhotoCraft, VectorCraft and DesignCraft as the Photo, Vector and Layout personas of
//! one window, switched from the persona bar.
//!
//! Usage: `almanak [files…]`. Each file opens in the persona that edits its kind (`.psd`, `.png`
//! → Photo; `.svg`, `.vectorcraft` → Vector; `.designcraft`, `.idml` → Layout).
//!
//! The three apps share one egui context: only the active persona runs its frame, and switching
//! re-installs its fonts and theme (`reinstall_style`), which the others replaced.

use std::path::Path;

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

struct Almanak {
    active: Persona,
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

fn photo_services() -> photocraft_ui_egui::Services {
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
    // ponytail: the essentials of PhotoCraft's native services (open, save, export, dialogs);
    // clipboard images, recovery, presets and the rest come with the shared-document work.
    photocraft_ui_egui::Services {
        import: Some(Box::new(|name: &str, bytes: &[u8], depth: usize| {
            photocraft_io::import_with_svg_group_depth(name, bytes, depth).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
        })),
        export: Some(Box::new(|doc: &photocraft_doc::Document, path: &str, s: &ExportSettings| {
            photocraft_io::export(doc, path, &export_options(s)).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
        })),
        save_file: Some(std::sync::Arc::new(|doc: &photocraft_doc::Document, path: &str, s: &ExportSettings, ctl: &SaveCtl| {
            ctl.progress(0.0, "Encoding");
            let r = photocraft_io::export(doc, path, &export_options(s)).map_err(|e| e.to_string())?;
            ctl.commit()?;
            ctl.progress(0.0, "Writing");
            photocraft_format::atomic_write(Path::new(path), &r.bytes).map_err(|e| e.to_string())?;
            Ok(r.warnings)
        })),
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
    let files: Vec<String> = std::env::args().skip(1).collect();
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
            let mut photo = PhotocraftApp::new(photocraft_engine::Session::new(), photo_services());
            if let Some(rs) = cc.wgpu_render_state.clone() {
                photo.set_wgpu(rs);
            }
            let mut app = Almanak {
                active: files.first().and_then(|f| Persona::for_path(f)).unwrap_or(Persona::Photo),
                photo,
                vector: VectorcraftApp::new(vectorcraft_engine::Session::new(), vector_services()),
                layout: DesignApp::new(designcraft_engine::Session::new(), layout_services()),
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

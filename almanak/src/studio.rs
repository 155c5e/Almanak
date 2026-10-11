//! StudioLink: a graphic placed in the Layout document is edited in place in the Photo or Vector
//! persona, and written back into the layout when Layout comes to the front again.
//!
//! Switching from Layout to Photo or Vector with a placed graphic selected opens that graphic's
//! embedded bytes there (raster → Photo, PDF/SVG → Vector). Switching back to Layout writes every
//! edited graphic back into its asset, one undo step each: Photo's as PSD (layers kept), Vector's
//! as PDF with Preserve Editing (the `.vectorcraft` document travels inside, so it reopens fully
//! editable). The layout's `.designcraft` file is the shared document: it embeds the assets.

use std::path::Path;
use std::sync::Arc;

use designcraft_doc::{AssetId, Content};
use designcraft_ui_egui::DesignApp;
use photocraft_ui_egui::PhotocraftApp;
use vectorcraft_ui_egui::VectorcraftApp;

use crate::Persona;

/// Where an embedded graphic is being edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// A PhotoCraft document (its `DocId`).
    Photo(u64),
    /// A VectorCraft document (its session uid).
    Vector(u64),
}

#[derive(Debug)]
struct Link {
    /// The layout document (DesignCraft session uid) and the asset in it.
    layout: u64,
    asset: AssetId,
    target: Target,
    /// The target document's revision last written back (or opened).
    synced: u64,
}

#[derive(Default)]
pub struct Studio {
    links: Vec<Link>,
}

/// The persona that edits a graphic of this MIME type.
fn persona_for_mime(mime: &str) -> Persona {
    match mime {
        "application/pdf" | "image/svg+xml" => Persona::Vector,
        _ => Persona::Photo,
    }
}

/// `name` with its extension replaced by `ext`.
fn renamed(name: &str, ext: &str) -> String {
    Path::new(name).with_extension(ext).to_string_lossy().into_owned()
}

impl Studio {
    /// Layout → `to`: open the selected placed graphic in `to` for editing, if it's one `to`
    /// edits. Returns what happened, for the status line.
    pub fn edit_selected(&mut self, to: Persona, layout: &DesignApp, photo: &mut PhotocraftApp, vector: &mut VectorcraftApp) -> Result<Option<String>, String> {
        let Some(ds) = layout.session.active() else { return Ok(None) };
        let graphic = ds.selection.items.iter().find_map(|id| match &ds.doc.item(*id)?.content {
            Content::Graphic(g) => Some(g.asset),
            _ => None,
        });
        let Some(asset) = graphic.and_then(|a| ds.doc.assets.get(&a)) else { return Ok(None) };
        if persona_for_mime(&asset.mime) != to {
            return Ok(None);
        }
        // Already open: bring its document to the front.
        if let Some(link) = self.links.iter().find(|l| l.layout == ds.uid && l.asset == asset.id) {
            let found = match link.target {
                Target::Photo(id) => photo.session.documents().iter().position(|d| d.doc.id.0 == id).map(|i| photo.session.set_active(i)),
                Target::Vector(uid) => vector.session.documents().iter().position(|d| d.uid == uid).map(|i| vector.session.set_active(i)),
            };
            if found.is_some() {
                return Ok(Some(format!("Editing {}", asset.name)));
            }
            // Its document was closed: open the graphic again.
            self.links.retain(|l| !(l.layout == ds.uid && l.asset == asset.id));
        }
        let (layout_uid, asset_id, name, bytes) = (ds.uid, asset.id, asset.name.clone(), asset.data.clone());
        let target = match to {
            Persona::Photo => {
                photo.open_bytes(&name, &bytes)?;
                let d = photo.session.active().ok_or("the graphic didn't open")?;
                (Target::Photo(d.doc.id.0), d.revision)
            }
            Persona::Vector => {
                vectorcraft_ui_egui::io::open_bytes(vector, &name, &bytes, None)?;
                let d = vector.session.active().ok_or("the graphic didn't open")?;
                (Target::Vector(d.uid), d.revision)
            }
            Persona::Layout => return Ok(None),
        };
        self.links.push(Link { layout: layout_uid, asset: asset_id, target: target.0, synced: target.1 });
        Ok(Some(format!("Editing {name} from the layout")))
    }

    /// Write every graphic edited since it was opened (or last written) back into its layout
    /// asset. Links whose documents were closed are dropped. Returns how many were updated.
    pub fn write_back(&mut self, layout: &mut DesignApp, photo: &PhotocraftApp, vector: &VectorcraftApp) -> Result<usize, String> {
        let mut updated = 0;
        let mut errors = Vec::new();
        self.links.retain_mut(|link| {
            // The edited document's current bytes, or `None` when it is unchanged; drop the link
            // when the document is gone.
            let fresh = match link.target {
                Target::Photo(id) => {
                    let Some(d) = photo.session.documents().iter().find(|d| d.doc.id.0 == id) else { return false };
                    (d.revision != link.synced).then(|| {
                        let r = photocraft_io::export(&d.doc, "graphic.psd", &photocraft_io::ExportOptions::default()).map(|r| (r.bytes, "psd"));
                        (d.revision, r.map_err(|e| e.to_string()))
                    })
                }
                Target::Vector(uid) => {
                    let Some(d) = vector.session.documents().iter().find(|d| d.uid == uid) else { return false };
                    (d.revision != link.synced).then(|| {
                        let opts = vectorcraft_pdf::PdfOptions {
                            settings: vectorcraft_pdf::PdfSettings { preserve_editing: true, ..Default::default() },
                            native: Some(vectorcraft_format::save_file(&d.doc)),
                            ..Default::default()
                        };
                        (d.revision, vectorcraft_pdf::export(&d.doc, &opts).map(|b| (b, "pdf")).map_err(|e| e.to_string()))
                    })
                }
            };
            let Some((revision, bytes)) = fresh else { return true };
            match bytes.and_then(|(bytes, ext)| replace_asset(layout, link.layout, link.asset, bytes, ext)) {
                Ok(()) => {
                    link.synced = revision;
                    updated += 1;
                }
                Err(e) => errors.push(e),
            }
            true
        });
        match errors.is_empty() {
            true => Ok(updated),
            false => Err(errors.join("; ")),
        }
    }
}

/// Put `bytes` (a `.ext` file) in asset `asset` of layout document `uid`, as one undo step, and
/// resize the graphics showing it when its natural size changed.
fn replace_asset(layout: &mut DesignApp, uid: u64, asset: AssetId, bytes: Vec<u8>, ext: &str) -> Result<(), String> {
    let s = &mut layout.session;
    let index = s.documents().iter().position(|d| d.uid == uid).ok_or("the layout document was closed")?;
    let front = s.active_index();
    s.set_active(index);
    let pixels = designcraft_render::image_size(&bytes);
    let natural = designcraft_images::natural_size(&bytes);
    let result = s.edit(|d, _| {
        let a = d.assets.get_mut(&asset).ok_or_else(|| designcraft_engine::EngineError::Other("the graphic is no longer in the layout".into()))?;
        let a = Arc::make_mut(a);
        a.mime = designcraft_render::image_mime(&bytes).to_string();
        a.name = renamed(&a.name, ext);
        a.pixels = pixels;
        a.data = Arc::new(bytes);
        // The embedded copy is now the original: an old link would offer to replace the edit.
        a.link = None;
        if let Some(size) = natural {
            for id in d.all_items() {
                if let Some(item) = d.item_mut(id)
                    && let Content::Graphic(g) = &mut item.content
                    && g.asset == asset
                {
                    g.size = size;
                }
            }
        }
        Ok(())
    });
    if let Some(front) = front {
        s.set_active(front);
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_go_to_the_persona_that_edits_them() {
        assert_eq!(persona_for_mime("image/png"), Persona::Photo);
        assert_eq!(persona_for_mime("image/vnd.adobe.photoshop"), Persona::Photo);
        assert_eq!(persona_for_mime("application/pdf"), Persona::Vector);
        assert_eq!(persona_for_mime("image/svg+xml"), Persona::Vector);
        assert_eq!(renamed("photo.jpg", "psd"), "photo.psd");
        assert_eq!(renamed("logo", "pdf"), "logo.pdf");
    }
}

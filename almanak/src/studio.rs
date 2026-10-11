//! StudioLink: a graphic placed in the Layout document is edited in place in the Photo or Vector
//! persona, and written back into the layout when Layout comes to the front again.
//!
//! Switching from Layout to Photo or Vector with a placed graphic selected opens that graphic's
//! embedded bytes there (raster → Photo, PDF/SVG → Vector; the kind is read from the bytes, never
//! from the name the document gives them). Switching back to Layout writes every graphic edited
//! since into its asset, one undo step each: Photo's as PSD (layers kept), Vector's as PDF with
//! Preserve Editing (the `.vectorcraft` document travels inside, so it reopens fully editable).
//! The layout's `.designcraft` file is the shared document: it embeds the assets.

use std::path::Path;
use std::sync::Arc;

use designcraft_doc::{AssetId, Content, Document as LayoutDoc, Item};
use designcraft_geom::Affine;
use designcraft_ui_egui::DesignApp;
use photocraft_ui_egui::PhotocraftApp;
use vectorcraft_ui_egui::VectorcraftApp;

use crate::Persona;

/// Where an embedded graphic is being edited, and that document as last opened or written back.
#[derive(Clone, Debug)]
enum Target {
    /// A PhotoCraft document (its `DocId`).
    Photo(u64, Arc<photocraft_doc::Document>),
    /// A VectorCraft document (its session uid).
    Vector(u64, Arc<vectorcraft_doc::Document>),
}

#[derive(Debug)]
struct Link {
    /// The layout document (DesignCraft session uid) and the asset in it.
    layout: u64,
    asset: AssetId,
    /// The asset's bytes as opened or last written: other bytes there (Relink, Update Link)
    /// mean the edit is about an old version, so the link is dropped instead of written.
    bytes: Arc<Vec<u8>>,
    target: Target,
}

#[derive(Default)]
pub struct Studio {
    links: Vec<Link>,
}

/// The persona that edits graphics of this (sniffed) MIME type, and the file name it opens them
/// under; `None` for kinds neither edits.
fn editor_for(mime: &str) -> Option<(Persona, &'static str)> {
    Some(match mime {
        "application/pdf" => (Persona::Vector, "graphic.pdf"),
        "image/svg+xml" => (Persona::Vector, "graphic.svg"),
        "image/vnd.adobe.photoshop" => (Persona::Photo, "graphic.psd"),
        "image/png" => (Persona::Photo, "graphic.png"),
        "image/jpeg" => (Persona::Photo, "graphic.jpg"),
        "image/gif" => (Persona::Photo, "graphic.gif"),
        "image/webp" => (Persona::Photo, "graphic.webp"),
        "image/tiff" => (Persona::Photo, "graphic.tif"),
        "image/bmp" => (Persona::Photo, "graphic.bmp"),
        _ => return None,
    })
}

/// `name` with its extension replaced by `ext`.
fn renamed(name: &str, ext: &str) -> String {
    Path::new(name).with_extension(ext).to_string_lossy().into_owned()
}

/// Did the Photo document change in what it exports? (Its selection, guides and notes live in
/// the document too, and don't count.)
fn photo_edited(old: &photocraft_doc::Document, new: &photocraft_doc::Document) -> bool {
    !std::ptr::eq(old, new)
        && (old.layers != new.layers
            || old.size != new.size
            || old.mode != new.mode
            || old.depth != new.depth
            || old.icc_profile != new.icc_profile
            || old.resolution_dpi != new.resolution_dpi)
}

/// The first placed graphic in `item` (itself, or inside it when it's a group).
fn graphic_in(item: &Item) -> Option<AssetId> {
    let mut found = None;
    item.walk(&mut |i| {
        if found.is_none()
            && let Content::Graphic(g) = &i.content
        {
            found = Some(g.asset);
        }
    });
    found
}

impl Studio {
    /// Layout → `to`: open the selected placed graphic in `to` for editing, if it's one `to`
    /// edits. Returns what happened, for the status line.
    pub fn edit_selected(&mut self, to: Persona, layout: &DesignApp, photo: &mut PhotocraftApp, vector: &mut VectorcraftApp) -> Result<Option<String>, String> {
        let Some(ds) = layout.session.active() else { return Ok(None) };
        let graphics: Vec<AssetId> = ds.selection.items.iter().filter_map(|id| graphic_in(ds.doc.item(*id)?)).collect();
        // Of the selected graphics, the first that `to` edits.
        let Some((asset, name)) = graphics.iter().filter_map(|a| ds.doc.assets.get(a)).find_map(|a| {
            let (persona, name) = editor_for(designcraft_render::image_mime(&a.data))?;
            (persona == to).then_some((a, name))
        }) else {
            return Ok(None);
        };
        // Already open: bring its document to the front, unless the layout's bytes changed since.
        if let Some(i) = self.links.iter().position(|l| l.layout == ds.uid && l.asset == asset.id) {
            let link = &self.links[i];
            let shown = Arc::ptr_eq(&link.bytes, &asset.data)
                && match &link.target {
                    Target::Photo(id, _) => photo.session.documents().iter().position(|d| d.doc.id.0 == *id).map(|i| photo.session.set_active(i)).is_some(),
                    Target::Vector(uid, _) => vector.session.documents().iter().position(|d| d.uid == *uid).map(|i| vector.session.set_active(i)).is_some(),
                };
            if shown {
                return Ok(Some(format!("Editing {}", asset.name)));
            }
            // Its document was closed, or the layout has other bytes now: open them afresh.
            self.links.remove(i);
        }
        let (layout_uid, asset_id, title, bytes) = (ds.uid, asset.id, asset.name.clone(), asset.data.clone());
        // Only a document that this open added is linked: an open that shows a dialog instead
        // (pages of a PDF, say) leaves the front document as it was, which must not be linked.
        let target = match to {
            Persona::Photo => {
                let before: Vec<u64> = photo.session.documents().iter().map(|d| d.doc.id.0).collect();
                photo.open_bytes(name, &bytes)?;
                let d = photo.session.documents().iter().find(|d| !before.contains(&d.doc.id.0)).ok_or("the graphic didn't open as a document")?;
                Target::Photo(d.doc.id.0, d.doc.clone())
            }
            Persona::Vector => {
                let before: Vec<u64> = vector.session.documents().iter().map(|d| d.uid).collect();
                vectorcraft_engine::file_access::unconfined(|| vectorcraft_ui_egui::io::open_bytes(vector, name, &bytes, None))?;
                let d = vector.session.documents().iter().find(|d| !before.contains(&d.uid)).ok_or("the graphic didn't open as a document")?;
                Target::Vector(d.uid, d.doc.clone())
            }
            Persona::Layout => return Ok(None),
        };
        self.links.push(Link { layout: layout_uid, asset: asset_id, bytes, target });
        Ok(Some(format!("Editing {title} from the layout")))
    }

    /// Write every graphic edited since it was opened (or last written) back into its asset in the
    /// layout document in front, and count the Photo or Vector document as saved (its edit is in
    /// the layout now: closing it or quitting needn't ask about it). Links whose documents were
    /// closed are dropped; those of other layout documents wait until theirs is in front. Returns
    /// what happened, for the status line.
    pub fn write_back(&mut self, layout: &mut DesignApp, photo: &mut PhotocraftApp, vector: &mut VectorcraftApp) -> Result<Option<String>, String> {
        let open_layouts: Vec<u64> = layout.session.documents().iter().map(|d| d.uid).collect();
        let front = layout.session.active().map(|d| (d.uid, d.preview_stash.is_some()));
        let (mut updated, mut waiting) = (0, 0);
        let mut errors = Vec::new();
        self.links.retain_mut(|link| {
            if !open_layouts.contains(&link.layout) {
                return false;
            }
            // The edited document's bytes when it changed since; drop the link when it's gone.
            let fresh = match &link.target {
                Target::Photo(id, seen) => {
                    let Some(d) = photo.session.documents().iter().find(|d| d.doc.id.0 == *id) else { return false };
                    photo_edited(seen, &d.doc).then(|| {
                        let bytes = photocraft_io::export(&d.doc, "graphic.psd", &photocraft_io::ExportOptions::default()).map(|r| r.bytes).map_err(|e| e.to_string());
                        (Target::Photo(*id, d.doc.clone()), bytes, "psd")
                    })
                }
                Target::Vector(uid, seen) => {
                    let Some(d) = vector.session.documents().iter().find(|d| d.uid == *uid) else { return false };
                    (!Arc::ptr_eq(seen, &d.doc) && **seen != *d.doc).then(|| {
                        let opts = vectorcraft_pdf::PdfOptions {
                            settings: vectorcraft_pdf::PdfSettings { preserve_editing: true, ..Default::default() },
                            native: Some(vectorcraft_format::save_file(&d.doc)),
                            ..Default::default()
                        };
                        (Target::Vector(*uid, d.doc.clone()), vectorcraft_pdf::export(&d.doc, &opts).map_err(|e| e.to_string()), "pdf")
                    })
                }
            };
            let Some((target, bytes, ext)) = fresh else { return true };
            // Only into the layout document in front, and not over a data merge preview's records.
            if front != Some((link.layout, false)) {
                waiting += 1;
                return true;
            }
            let bytes = match bytes {
                Ok(b) => Arc::new(b),
                Err(e) => {
                    errors.push(e);
                    return true;
                }
            };
            match replace_asset(layout, link.asset, &link.bytes, bytes.clone(), ext) {
                Ok(true) => {
                    mark_saved(&target, photo, vector);
                    link.bytes = bytes;
                    link.target = target;
                    updated += 1;
                    true
                }
                // The layout has other bytes there now: this edit is about an old version.
                Ok(false) => false,
                Err(e) => {
                    errors.push(e);
                    true
                }
            }
        });
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        Ok(match (updated, waiting) {
            (0, 0) => None,
            (n, 0) => Some(format!("Updated {n} graphic{} in the layout", plural(n))),
            (n, w) => Some(format!("Updated {n} graphic{}; {w} wait for their layout to be in front", plural(n))),
        })
    }
}

/// Count the document `target` names, as it was exported, as saved.
fn mark_saved(target: &Target, photo: &mut PhotocraftApp, vector: &mut VectorcraftApp) {
    match target {
        Target::Photo(id, exported) => {
            // Only the front document can be changed: bring it forward for this, then put the
            // front one back.
            let front = photo.session.active_index();
            if let Some(i) = photo.session.documents().iter().position(|d| d.doc.id.0 == *id)
                && photo.session.set_active(i)
                && let Some(d) = photo.session.active_mut()
                && Arc::ptr_eq(&d.doc, exported)
            {
                d.saved_revision = d.revision;
            }
            if let Some(front) = front {
                photo.session.set_active(front);
            }
        }
        Target::Vector(uid, exported) => {
            if let Some(d) = vector.session.document_mut(*uid) {
                d.mark_saved_as(exported);
            }
        }
    }
}

/// Put `bytes` (a `.ext` file) in asset `asset` of the layout document in front, as one undo
/// step, keeping the graphics showing it the same size on the page. `Ok(false)`: the asset no
/// longer holds `expected` (relinked or updated meanwhile), so nothing is written.
fn replace_asset(layout: &mut DesignApp, asset: AssetId, expected: &Arc<Vec<u8>>, bytes: Arc<Vec<u8>>, ext: &str) -> Result<bool, String> {
    let s = &mut layout.session;
    let st = s.doc_mut().map_err(|e| e.to_string())?;
    match st.doc.assets.get(&asset) {
        Some(a) if Arc::ptr_eq(&a.data, expected) => {}
        _ => return Ok(false),
    }
    let (before, selection) = (st.doc.clone(), st.selection.clone());
    let pixels = designcraft_render::image_size(&bytes);
    let natural = designcraft_images::natural_size(&bytes);
    s.edit(|d, _| {
        let a = d.assets.get_mut(&asset).ok_or_else(|| designcraft_engine::EngineError::Other("the graphic is no longer in the layout".into()))?;
        let a = Arc::make_mut(a);
        a.mime = designcraft_render::image_mime(&bytes).to_string();
        a.name = renamed(&a.name, ext);
        a.pixels = pixels;
        a.data = bytes;
        // A page of the old PDF means nothing in the new one.
        a.page = 0;
        // The embedded copy is now the original: an old link would offer to replace the edit.
        a.link = None;
        if let Some(size) = natural {
            resize_graphics(d, asset, size);
        }
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    // `Session::edit` records no undo step: add one, as commands do.
    let st = s.doc_mut().map_err(|e| e.to_string())?;
    st.history.undo.push(designcraft_engine::HistoryEntry { label: "Edit Graphic".into(), doc: before, selection });
    st.history.redo.clear();
    if st.history.undo.len() > st.history.limit.max(1) {
        st.history.undo.remove(0);
    }
    Ok(true)
}

/// Give the graphics showing `asset` (spreads and parent pages) their new natural `size`, scaled
/// to fit where the old one was so they keep their place and size on the page.
fn resize_graphics(d: &mut LayoutDoc, asset: AssetId, size: (f64, f64)) {
    let mut ids = Vec::new();
    for sp in d.spreads.iter().chain(&d.parents) {
        for it in &sp.items {
            it.walk(&mut |i| {
                if let Content::Graphic(g) = &i.content
                    && g.asset == asset
                    && g.size != size
                {
                    ids.push(i.id);
                }
            });
        }
    }
    // Only the matching items are copied (the others stay shared with the undo step).
    for id in ids {
        if let Some(item) = d.item_mut(id)
            && let Content::Graphic(g) = &mut item.content
        {
            g.xf = g.xf * Affine::scale(fit_scale(g.size, size));
            g.size = size;
        }
    }
}

/// The uniform scale that fits a graphic of natural size `new` where one of size `old` was.
fn fit_scale(old: (f64, f64), new: (f64, f64)) -> f64 {
    let s = (old.0 / new.0).min(old.1 / new.1);
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphics_go_to_the_persona_that_edits_them_by_their_bytes() {
        assert_eq!(editor_for("image/png"), Some((Persona::Photo, "graphic.png")));
        assert_eq!(editor_for("image/vnd.adobe.photoshop").map(|e| e.0), Some(Persona::Photo));
        assert_eq!(editor_for("application/pdf"), Some((Persona::Vector, "graphic.pdf")));
        assert_eq!(editor_for("image/svg+xml").map(|e| e.0), Some(Persona::Vector));
        // Bytes neither edits (a WASM plug-in, say) open nowhere, whatever the document calls them.
        assert_eq!(editor_for(designcraft_render::image_mime(b"\0asm\x01\0\0\0")), None);
        assert_eq!(renamed("photo.jpg", "psd"), "photo.psd");
        assert_eq!(renamed("logo", "pdf"), "logo.pdf");
    }

    #[test]
    fn a_resized_graphic_keeps_its_place_on_the_page() {
        // Twice the pixels: drawn at half the scale, the same size on the page.
        assert_eq!(fit_scale((300.0, 200.0), (600.0, 400.0)), 0.5);
        // A wider canvas fits inside the old bounds, undistorted.
        assert_eq!(fit_scale((300.0, 200.0), (600.0, 200.0)), 0.5);
        assert_eq!(fit_scale((300.0, 200.0), (0.0, 0.0)), 1.0);
    }

    #[test]
    fn selection_changes_are_not_photo_edits() {
        let a = Arc::new(photocraft_doc::Document::new("t", photocraft_doc::Size::new(4, 4), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8));
        let mut b = (*a).clone();
        b.selection = None;
        b.guides = Default::default();
        assert!(!photo_edited(&a, &b));
        b.size = photocraft_doc::Size::new(8, 4);
        assert!(photo_edited(&a, &b));
    }
}

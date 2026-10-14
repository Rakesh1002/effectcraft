//! Project fonts: `.ttf` / `.otf` files in the file table under `/fonts/` (a host's `putFile`, an
//! imported or dropped font file, stored files on the next visit) are registered with the font
//! database, so text layers can use their families (`text.fonts` lists them with origin
//! `project`). Workers get the same files with the footage they are sent and register them too.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use effectcraft_engine::text::fonts::{self, FaceId};
use serde_json::{Value, json};

pub const DIR: &str = "/fonts/";

/// Registered font files: table path → their faces.
static REGISTERED: Mutex<BTreeMap<String, Vec<FaceId>>> = Mutex::new(BTreeMap::new());
/// Bumped whenever the registered fonts change.
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The font set's generation: a frame rendered while it changed may show the fallback font.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

/// A font file a project brings: `/fonts/<name>.ttf` or `.otf`.
pub fn is_font_path(path: &str) -> bool {
    path.strip_prefix(DIR).is_some_and(|name| !name.is_empty() && fonts::is_font_file(name))
}

/// Register the font file at `path`: its faces (`[{path, family, style, origin}]`) and whether
/// the font database changed (registering the same bytes again changes nothing).
pub fn register(path: &str, data: &[u8]) -> Result<(Vec<Value>, bool), String> {
    let (ids, changed) = fonts::add_project_font(data.to_vec()).map_err(|e| format!("{path}: {e}"))?;
    if changed {
        GENERATION.fetch_add(1, Ordering::AcqRel);
    }
    REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), ids.clone());
    Ok((ids.iter().map(|&id| face_json(path, id)).collect(), changed))
}

fn face_json(path: &str, id: FaceId) -> Value {
    let f = fonts::face(id);
    json!({"path": path, "family": f.info.family, "style": f.info.style, "origin": f.info.origin})
}

/// Every registered project font face, by file.
pub fn list() -> Vec<Value> {
    REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).iter().flat_map(|(path, ids)| ids.iter().map(|&id| face_json(path, id))).collect()
}

/// The table paths of the registered font files (sent to workers with their footage).
pub fn paths() -> Vec<String> {
    REGISTERED.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
}

#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(target_arch = "wasm32")]
mod web {
    use effectcraft_ui_egui::EffectcraftApp;
    use serde_json::{Value, json};

    /// After a font file was registered on the page: drop the renders made without it (when the
    /// database changed) and raise `effectcraft:fonts-changed {path, faces, changed, fonts}`.
    pub fn changed(path: &str, faces: Vec<Value>, changed: bool) {
        if changed && crate::with_app(invalidate).is_none() {
            crate::post(invalidate);
        }
        crate::embed::emit_json("effectcraft:fonts-changed", &json!({"path": path, "faces": faces, "changed": changed, "fonts": super::list()}));
    }

    fn invalidate(app: &mut EffectcraftApp) {
        app.session.layer_cache.clear();
        app.frames.clear();
        // Disk cache keys hash the project, not its fonts.
        if crate::diskcache::active() {
            let _ = app.session.execute("edit.purge", json!({"what": "disk"}));
        }
        crate::repaint();
    }

    /// Register the font files stored in the table (browser storage on the next visit).
    pub fn register_stored() {
        for (path, _) in crate::files::list() {
            if super::is_font_path(&path)
                && let Some(data) = crate::files::get(&path)
                && let Err(e) = super::register(&path, &data)
            {
                log::warn!("{e}");
            }
        }
    }

    /// An imported or dropped font file (`/<name>.ttf`): moved to `/fonts/<name>` and registered.
    pub fn adopt(app: &mut EffectcraftApp, path: &str) {
        let name = path.rsplit('/').next().unwrap_or(path);
        let target = format!("{}{name}", super::DIR);
        let Some(data) = crate::files::get(path) else { return };
        match super::register(&target, &data) {
            Ok((faces, changed)) => {
                crate::files::put(&target, data);
                crate::files::STORE.remove_file(path);
                self::changed(&target, faces, changed);
            }
            Err(e) => app.ui.status = e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCKS: &[u8] = include_bytes!("../../../crates/text/tests/fixtures/EffectCraftTestBlocks.ttf");

    #[test]
    fn font_paths_are_ttf_and_otf_under_fonts() {
        assert!(is_font_path("/fonts/Brand.ttf") && is_font_path("/fonts/Brand-Bold.OTF") && is_font_path("/fonts/sub/a.ttf"));
        assert!(!is_font_path("/Brand.ttf") && !is_font_path("/fonts/") && !is_font_path("/fonts/.ttf.png") && !is_font_path("/fonts/a.woff2"));
        assert!(!is_font_path("/assets/fonts/a.ttf"));
    }

    #[test]
    fn registering_is_idempotent_and_listed() {
        let (faces, _) = register("/fonts/blocks.ttf", BLOCKS).unwrap();
        assert_eq!(faces, vec![json!({"path": "/fonts/blocks.ttf", "family": "EffectCraft Test Blocks", "style": "Regular", "origin": "project"})]);
        let (again, changed) = register("/fonts/blocks.ttf", BLOCKS).unwrap();
        assert_eq!((again, changed), (faces.clone(), false), "a no-op leaves the generation");
        assert!(list().contains(&faces[0]));
        assert!(paths().contains(&"/fonts/blocks.ttf".to_string()));
    }

    #[test]
    fn a_changed_font_set_bumps_the_generation() {
        let mut other = BLOCKS.to_vec();
        let at = other.windows(6).position(|w| w == b"\0B\0l\0o").unwrap();
        other[at + 1] = b'Q';
        let before = generation();
        let (_, changed) = register("/fonts/other.ttf", &other).unwrap();
        assert!(changed && generation() > before);
    }

    #[test]
    fn a_malformed_font_is_an_error_and_not_listed() {
        let e = register("/fonts/broken.ttf", b"\0\x01\0\0garbage").unwrap_err();
        assert!(e.starts_with("/fonts/broken.ttf: "), "{e}");
        assert!(!paths().contains(&"/fonts/broken.ttf".to_string()));
    }
}

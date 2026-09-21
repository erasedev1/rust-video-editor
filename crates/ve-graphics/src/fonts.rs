//! Which file a family name resolves to.
//!
//! # A project names a family, not a file
//!
//! A `.verge` opened on another machine has to find Helvetica rather than fail
//! to open a path that was never part of the edit — so the model carries a
//! family, a weight and an italic flag ([`ve_core::FontSpec`]), and this is
//! where that becomes a face. What happens when the family is not installed is
//! decided here too: it substitutes and says so, because a title that draws
//! nothing is worse than a title in the wrong typeface, and silence about it is
//! worse than both.
//!
//! # Scanned once for the machine, not once per editor
//!
//! Reading every font on a system takes long enough to notice, and the answer
//! is a property of the machine rather than of anything the editor is doing. So
//! the system library is built once behind a [`OnceLock`] and shared by the
//! preview and by every export. The app warms it on a background thread at
//! startup, so the first title anyone types is not also the first font scan.
//!
//! A library can also be built from nothing and pointed at particular files,
//! which is what tests do: a test that asserts pixels must not depend on which
//! fonts the machine it runs on happens to have.

use std::sync::{Arc, OnceLock};

use ve_core::FontSpec;

/// The fonts available to draw with.
pub struct FontLibrary {
    db: fontdb::Database,
}

/// Candidates for each generic family, in preference order.
///
/// `fontdb` defaults its generic families to the names Windows and macOS use,
/// which resolve to nothing at all on a bare Linux machine — so "sans-serif"
/// would find no face and a title would draw nothing. These are the families
/// actually shipped by the systems the editor runs on, and the first one
/// present wins.
const SANS: &[&str] =
    &["Helvetica", "Arial", "Liberation Sans", "DejaVu Sans", "Noto Sans", "Segoe UI"];
const SERIF: &[&str] =
    &["Times New Roman", "Liberation Serif", "DejaVu Serif", "Noto Serif", "Georgia"];
const MONO: &[&str] = &[
    "Menlo",
    "Consolas",
    "Liberation Mono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Courier New",
];

impl FontLibrary {
    /// A library holding nothing. Text drawn against it draws nothing.
    pub fn empty() -> Self {
        FontLibrary { db: fontdb::Database::new() }
    }

    /// Every font installed on this machine.
    pub fn system() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut library = FontLibrary { db };
        library.point_generics_at_what_is_installed();
        library
    }

    /// The system library, scanned at most once per process.
    pub fn shared() -> Arc<FontLibrary> {
        static SHARED: OnceLock<Arc<FontLibrary>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(FontLibrary::system())))
    }

    /// Adds a font file, for a test or for a project that ships its own.
    ///
    /// Returns whether anything was read; a file that is not a font is a
    /// refusal rather than a panic.
    pub fn load_file(&mut self, path: impl AsRef<std::path::Path>) -> bool {
        match std::fs::read(path.as_ref()) {
            Ok(data) => {
                let before = self.db.len();
                self.db.load_font_data(data);
                let loaded = self.db.len() > before;
                if loaded {
                    self.point_generics_at_what_is_installed();
                }
                loaded
            }
            Err(err) => {
                log::warn!("could not read font {}: {err}", path.as_ref().display());
                false
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.db.len() == 0
    }

    /// The family names available, sorted and without duplicates, for a menu.
    pub fn families(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .db
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// The face a spec asks for, or the nearest thing installed.
    ///
    /// Three steps, each one a fallback from the last: the family as asked for,
    /// then the generic sans serif, then whatever face the machine has. The
    /// last is deliberate — a machine with one font should still draw titles —
    /// and every step past the first is reported, because a user looking at the
    /// wrong typeface deserves to be told which one they are looking at.
    pub(crate) fn resolve(&self, spec: &FontSpec) -> Option<Resolved> {
        let style = if spec.italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
        let weight = fontdb::Weight(spec.weight);

        let family =
            generic_for(&spec.family).unwrap_or(fontdb::Family::Name(spec.family.as_str()));
        let query = fontdb::Query {
            families: &[family],
            weight,
            stretch: fontdb::Stretch::Normal,
            style,
        };
        if let Some(id) = self.db.query(&query) {
            return Some(Resolved { id, substituted: false });
        }

        let fallback = fontdb::Query {
            families: &[fontdb::Family::SansSerif],
            weight,
            stretch: fontdb::Stretch::Normal,
            style,
        };
        if let Some(id) = self.db.query(&fallback) {
            log::warn!("no font for '{}'; drawing with the sans serif", spec.family);
            return Some(Resolved { id, substituted: true });
        }

        let any = self.db.faces().next()?.id;
        log::warn!("no font for '{}'; drawing with whatever is installed", spec.family);
        Some(Resolved { id: any, substituted: true })
    }

    /// Runs `f` over the raw bytes of a face.
    ///
    /// The bytes cannot outlive the database, so everything that reads them —
    /// shaping, outlining, measuring — happens inside the closure rather than
    /// being handed back.
    pub(crate) fn with_face_data<T>(
        &self,
        id: fontdb::ID,
        f: impl FnOnce(&[u8], u32) -> T,
    ) -> Option<T> {
        self.db.with_face_data(id, f)
    }

    /// Points the generic families at something that actually exists here.
    fn point_generics_at_what_is_installed(&mut self) {
        if let Some(name) = self.first_installed(SANS) {
            self.db.set_sans_serif_family(name);
        }
        if let Some(name) = self.first_installed(SERIF) {
            self.db.set_serif_family(name);
        }
        if let Some(name) = self.first_installed(MONO) {
            self.db.set_monospace_family(name);
        }
    }

    fn first_installed(&self, candidates: &[&str]) -> Option<String> {
        let installed = |name: &str| {
            self.db.faces().any(|face| {
                face.families.iter().any(|(family, _)| family.eq_ignore_ascii_case(name))
            })
        };
        candidates
            .iter()
            .find(|name| installed(name))
            .map(|name| (*name).to_string())
            // Nothing recognised: any family at all beats none, because the
            // alternative is that every generic draws nothing.
            .or_else(|| self.db.faces().next()?.families.first().map(|(name, _)| name.clone()))
    }
}

impl Default for FontLibrary {
    fn default() -> Self {
        FontLibrary::empty()
    }
}

impl std::fmt::Debug for FontLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontLibrary").field("faces", &self.db.len()).finish()
    }
}

/// A face, and whether it is the one that was asked for.
pub(crate) struct Resolved {
    pub id: fontdb::ID,
    #[allow(dead_code)]
    pub substituted: bool,
}

/// The CSS generic families, which name a role rather than a typeface.
fn generic_for(family: &str) -> Option<fontdb::Family<'static>> {
    match family.to_ascii_lowercase().as_str() {
        "sans-serif" => Some(fontdb::Family::SansSerif),
        "serif" => Some(fontdb::Family::Serif),
        "monospace" => Some(fontdb::Family::Monospace),
        "cursive" => Some(fontdb::Family::Cursive),
        "fantasy" => Some(fontdb::Family::Fantasy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_library_resolves_nothing() {
        let library = FontLibrary::empty();
        assert!(library.is_empty());
        assert!(library.resolve(&FontSpec::default()).is_none());
        assert!(library.families().is_empty());
    }

    #[test]
    fn a_family_that_is_not_installed_substitutes_rather_than_failing() {
        let library = crate::test_library();
        let resolved = library
            .resolve(&FontSpec::new("Nothing Is Called This 9000"))
            .expect("something to draw with");
        assert!(resolved.substituted);
    }

    #[test]
    fn the_generic_sans_serif_resolves_to_something_installed() {
        let library = crate::test_library();
        let resolved = library.resolve(&FontSpec::default()).expect("a sans serif");
        assert!(!resolved.substituted, "the generic families are pointed at real ones");
    }
}

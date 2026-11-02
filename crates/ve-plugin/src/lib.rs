//! Plugins: how a third party adds an effect without the core knowing it.
//!
//! A plugin is a directory holding a `plugin.json` manifest. The manifest
//! declares effects — their kind key, menu entry and parameters — in exactly
//! the terms the [`EffectRegistry`] already uses, so a plugin's effect shows in
//! the menu, gets inspector controls and keyframes like any built-in, with no
//! change anywhere downstream.
//!
//! This crate is the *declaration* half of the plugin boundary: parsing,
//! validating and registering. It never runs plugin code. Giving a declared
//! effect a program to run is the renderer's side of the boundary; until a
//! plugin supplies one, its effect is registered but skipped at render time,
//! which is the same, already-tested path an effect from an uninstalled plugin
//! takes.
//!
//! # Rules a manifest must follow
//!
//! * `api_version` must equal [`API_VERSION`]. The format is not yet stable, so
//!   there is no range to be lenient about.
//! * Every effect kind must start with `<plugin id>.` — the namespace is what
//!   stops a plugin taking a name the editor, or another plugin, owns.
//! * A kind may not collide with a built-in or with another plugin's.
//! * Parameter keys are unique within an effect, and ranges must be ordered
//!   with the default inside them.
//!
//! A plugin that breaks a rule is rejected whole: half a plugin registered
//! would make a project's rendering depend on which half.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ve_core::geometry::Vec2;
use ve_core::{
    EffectCategory, EffectDescriptor, EffectRegistry, ParamDescriptor, ParamKind, Rgba,
};

/// The manifest format this build reads.
pub const API_VERSION: u32 = 1;

/// The file name looked for inside a plugin directory.
pub const MANIFEST_FILE: &str = "plugin.json";

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("invalid manifest: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("plugin targets API version {found}, this build reads {}", API_VERSION)]
    ApiVersion { found: u32 },
    #[error("plugin id {0:?} must be lowercase letters, digits, '-' or '_', and non-empty")]
    BadId(String),
    #[error("effect kind {kind:?} must start with \"{id}.\"")]
    NotNamespaced { kind: String, id: String },
    #[error("effect kind {0:?} is already registered")]
    Duplicate(String),
    #[error("effect {kind:?}: {problem}")]
    BadEffect { kind: String, problem: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CategoryDecl {
    Blur,
    Color,
    Stylize,
    Distort,
    Matte,
}

impl From<CategoryDecl> for EffectCategory {
    fn from(c: CategoryDecl) -> Self {
        match c {
            CategoryDecl::Blur => EffectCategory::Blur,
            CategoryDecl::Color => EffectCategory::Color,
            CategoryDecl::Stylize => EffectCategory::Stylize,
            CategoryDecl::Distort => EffectCategory::Distort,
            CategoryDecl::Matte => EffectCategory::Matte,
        }
    }
}

/// A parameter's type, bounds and default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ParamDecl {
    Scalar { default: f64, min: f64, max: f64 },
    Point { default: [f64; 2], min: f64, max: f64 },
    Color { default: [f64; 4] },
    Bool { default: bool },
    Choice { default: u32, options: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamEntry {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub hint: String,
    #[serde(flatten)]
    pub decl: ParamDecl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectEntry {
    pub kind: String,
    pub name: String,
    pub category: CategoryDecl,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub params: Vec<ParamEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub effects: Vec<EffectEntry>,
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Manifest, PluginError> {
        let manifest: Manifest = serde_json::from_str(json)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn load(dir: &Path) -> Result<Manifest, PluginError> {
        let path = dir.join(MANIFEST_FILE);
        let text =
            fs::read_to_string(&path).map_err(|source| PluginError::Io { path, source })?;
        Manifest::parse(&text)
    }

    pub fn validate(&self) -> Result<(), PluginError> {
        if self.api_version != API_VERSION {
            return Err(PluginError::ApiVersion { found: self.api_version });
        }
        let id_ok = !self.id.is_empty()
            && self
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !id_ok {
            return Err(PluginError::BadId(self.id.clone()));
        }
        let prefix = format!("{}.", self.id);
        let mut seen = Vec::new();
        for effect in &self.effects {
            if !effect.kind.starts_with(&prefix) || effect.kind.len() == prefix.len() {
                return Err(PluginError::NotNamespaced {
                    kind: effect.kind.clone(),
                    id: self.id.clone(),
                });
            }
            if seen.contains(&&effect.kind) {
                return Err(PluginError::Duplicate(effect.kind.clone()));
            }
            seen.push(&effect.kind);
            effect.validate()?;
        }
        Ok(())
    }

    /// Registers every effect, or none: collisions are checked first.
    pub fn register(&self, registry: &mut EffectRegistry) -> Result<(), PluginError> {
        self.validate()?;
        if let Some(e) = self.effects.iter().find(|e| registry.contains(&e.kind)) {
            return Err(PluginError::Duplicate(e.kind.clone()));
        }
        for effect in &self.effects {
            registry.register(effect.descriptor());
        }
        Ok(())
    }
}

impl EffectEntry {
    fn bad(&self, problem: impl Into<String>) -> PluginError {
        PluginError::BadEffect { kind: self.kind.clone(), problem: problem.into() }
    }

    fn validate(&self) -> Result<(), PluginError> {
        let mut keys: Vec<&str> = Vec::new();
        for p in &self.params {
            if p.key.is_empty() || keys.contains(&p.key.as_str()) {
                return Err(self.bad(format!("parameter key {:?} is empty or repeated", p.key)));
            }
            keys.push(&p.key);
            match &p.decl {
                ParamDecl::Scalar { default, min, max } => {
                    check_range(self, &p.key, *default, *min, *max)?
                }
                ParamDecl::Point { default, min, max } => {
                    check_range(self, &p.key, default[0], *min, *max)?;
                    check_range(self, &p.key, default[1], *min, *max)?;
                }
                ParamDecl::Choice { default, options } => {
                    if *default as usize >= options.len() {
                        return Err(self.bad(format!("{:?}: default is not an option", p.key)));
                    }
                }
                ParamDecl::Color { .. } | ParamDecl::Bool { .. } => {}
            }
        }
        Ok(())
    }

    fn descriptor(&self) -> EffectDescriptor {
        let mut d =
            EffectDescriptor::new(&self.kind, &self.name, self.category.into(), &self.summary);
        for p in &self.params {
            let kind = match &p.decl {
                ParamDecl::Scalar { default, min, max } => {
                    ParamKind::scalar(*default, *min, *max)
                }
                ParamDecl::Point { default, min, max } => {
                    ParamKind::point(Vec2::new(default[0], default[1]), *min, *max)
                }
                ParamDecl::Color { default } => ParamKind::Color {
                    default: Rgba::new(default[0], default[1], default[2], default[3]),
                },
                ParamDecl::Bool { default } => ParamKind::Bool { default: *default },
                ParamDecl::Choice { default, options } => {
                    ParamKind::Choice { default: *default, options: options.clone() }
                }
            };
            d = d.with_param(ParamDescriptor::new(&p.key, &p.label, kind, &p.hint));
        }
        d
    }
}

fn check_range(
    e: &EffectEntry,
    key: &str,
    default: f64,
    min: f64,
    max: f64,
) -> Result<(), PluginError> {
    let finite = default.is_finite() && min.is_finite() && max.is_finite();
    if !finite || min > max || default < min || default > max {
        return Err(e.bad(format!("{key:?}: need min <= default <= max, all finite")));
    }
    Ok(())
}

/// Loads every plugin directory under `root` into `registry`.
///
/// A plugin that fails is reported and skipped; the others still load. The
/// result lists each directory with the outcome, in name order so it is stable.
pub fn load_dir(
    root: &Path,
    registry: &mut EffectRegistry,
) -> Vec<(PathBuf, Result<String, PluginError>)> {
    let Ok(read) = fs::read_dir(root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> =
        read.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.into_iter()
        .map(|dir| {
            let outcome = Manifest::load(&dir).and_then(|m| {
                m.register(registry)?;
                Ok(m.id)
            });
            (dir, outcome)
        })
        .collect()
}

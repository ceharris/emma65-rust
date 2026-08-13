//! Registry of bundled "starter profile" templates: named, self-contained
//! bundles of a ROM image, VICE labels file, and device-layout template.
//! Adding a template is additive: create `templates/<id>/` with its three
//! asset files plus a `mod.rs` calling [`asset::materialize`], then add one
//! [`Template`] entry to [`TEMPLATES`] below. No existing template's code or
//! files change.
pub(super) mod asset;
mod msbasic;

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

pub use asset::MaterializeError;

/// One bundled starter-profile template.
pub struct Template {
    /// Stable identifier used by the `emma65` binary's `--profile` flag and
    /// the debugger's New Profile template picker. `"default"` names the
    /// TaliForth2 bundle and must never change: the CLI's zero-config
    /// fallback and the debugger's `default` profile both depend on it.
    pub id: &'static str,
    /// Human-readable name shown in the New Profile template picker.
    pub name: &'static str,
    /// One-line description shown alongside `name` in the picker.
    pub description: &'static str,
    materialize_fn: fn(&Path) -> Result<PathBuf, MaterializeError>,
}

impl Template {
    /// Writes this template's bundled assets into `dest` (created if
    /// missing). Returns the path to the written `emulator.toml`.
    pub fn materialize(&self, dest: &Path) -> Result<PathBuf, MaterializeError> {
        (self.materialize_fn)(dest)
    }
}

/// Every bundled starter-profile template, in display order.
pub static TEMPLATES: &[Template] = &[
    Template {
        id: "default",
        name: "TaliForth2",
        description: "Forth-2012 system for the 65C02 with VIA, ACIA, and PTM I/O devices.",
        materialize_fn: super::default::materialize_default_config,
    },
    Template {
        id: "msbasic",
        name: "MS BASIC",
        description: "Microsoft 6502 BASIC interpreter with RAM, ROM, and a console device.",
        materialize_fn: msbasic::materialize_msbasic_config,
    },
];

/// Looks up a bundled template by its stable `id`.
pub fn find(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// Names a template `id` that has no entry in [`TEMPLATES`].
#[derive(Debug)]
pub struct UnknownTemplateError {
    /// The requested, unregistered template id.
    pub id: String,
}

impl Display for UnknownTemplateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<_> = TEMPLATES.iter().map(|t| t.id).collect();
        write!(f, "unknown starter-profile template '{}' (available: {})", self.id, ids.join(", "))
    }
}

impl std::error::Error for UnknownTemplateError {}

/// An error materializing a starter-profile template: either its `id` names
/// no registered template, or writing its assets failed.
#[derive(Debug)]
pub enum TemplateError {
    /// `id` has no entry in [`TEMPLATES`].
    Unknown(UnknownTemplateError),
    /// The template was found but its assets could not be written.
    Materialize(MaterializeError),
}

impl Display for TemplateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::Unknown(e) => Display::fmt(e, f),
            TemplateError::Materialize(e) => Display::fmt(e, f),
        }
    }
}

impl std::error::Error for TemplateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TemplateError::Unknown(e) => Some(e),
            TemplateError::Materialize(e) => Some(e),
        }
    }
}

/// Materializes template `id`'s bundled assets into `dest` (created if
/// missing). Returns the path to the written `emulator.toml`.
pub fn materialize(id: &str, dest: &Path) -> Result<PathBuf, TemplateError> {
    find(id)
        .ok_or_else(|| TemplateError::Unknown(UnknownTemplateError { id: id.to_string() }))?
        .materialize(dest)
        .map_err(TemplateError::Materialize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dest(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("emma65-templates-registry-test-{name}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn find_returns_registered_templates() {
        assert!(find("default").is_some());
        assert!(find("msbasic").is_some());
    }

    #[test]
    fn find_returns_none_for_unregistered_id() {
        assert!(find("nope").is_none());
    }

    #[test]
    fn registered_template_ids_are_exactly_default_and_msbasic() {
        let ids: Vec<_> = TEMPLATES.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["default", "msbasic"]);
    }

    #[test]
    fn materialize_unknown_id_names_it_in_the_error() {
        let dest = temp_dest("unknown-id");
        let err = materialize("nope", &dest).unwrap_err();
        assert!(matches!(err, TemplateError::Unknown(_)));
        assert!(err.to_string().contains("nope"));
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn materialize_msbasic_writes_its_emulator_toml() {
        let dest = temp_dest("msbasic");
        let toml_path = materialize("msbasic", &dest).unwrap();
        assert_eq!(toml_path, dest.join("emulator.toml"));
        let _ = std::fs::remove_dir_all(&dest);
    }
}

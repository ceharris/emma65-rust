use std::fmt;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A [`PathBuf`] that expands a leading `~/` to the user's home directory on
/// construction. Both `FromStr` and `Deserialize` perform expansion, so paths
/// arriving via CLI arguments or TOML/env-var configuration are treated the same.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExpandedPathBuf(PathBuf);

impl ExpandedPathBuf {

    /// Expands `~/` at the start of `s` to `$HOME/`, then wraps the result.
    pub fn new(s: &str) -> Self {
        ExpandedPathBuf(expand(s))
    }

}

impl Deref for ExpandedPathBuf {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<Path> for ExpandedPathBuf {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for ExpandedPathBuf {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
    }
}

impl From<ExpandedPathBuf> for PathBuf {
    fn from(p: ExpandedPathBuf) -> Self {
        p.0
    }
}

impl fmt::Display for ExpandedPathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

impl FromStr for ExpandedPathBuf {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(ExpandedPathBuf::new(s))
    }
}

impl Serialize for ExpandedPathBuf {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ExpandedPathBuf {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(ExpandedPathBuf::new(&s))
    }
}

fn expand(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~" && let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home);
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_slash() {
        unsafe { std::env::set_var("HOME", "/home/user") };
        let p = ExpandedPathBuf::new("~/foo/bar");
        assert_eq!(p.0, PathBuf::from("/home/user/foo/bar"));
    }

    #[test]
    fn expand_bare_tilde() {
        unsafe { std::env::set_var("HOME", "/home/user") };
        let p = ExpandedPathBuf::new("~");
        assert_eq!(p.0, PathBuf::from("/home/user"));
    }

    #[test]
    fn no_expansion_for_absolute_path() {
        let p = ExpandedPathBuf::new("/etc/hosts");
        assert_eq!(p.0, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn no_expansion_for_relative_path() {
        let p = ExpandedPathBuf::new("relative/path");
        assert_eq!(p.0, PathBuf::from("relative/path"));
    }

    #[test]
    fn from_str_expands() {
        unsafe { std::env::set_var("HOME", "/home/user") };
        let p: ExpandedPathBuf = "~/roms/taliforth.bin".parse().unwrap();
        assert_eq!(p.0, PathBuf::from("/home/user/roms/taliforth.bin"));
    }

    #[test]
    fn deref_to_path() {
        let p = ExpandedPathBuf::new("/etc/hosts");
        assert_eq!(p.file_name().unwrap(), "hosts");
    }
}

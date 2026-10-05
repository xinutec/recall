/// A recorder's or a session's id: `[a-z0-9][a-z0-9_-]*`, at most 64 bytes.
///
/// The id is a directory name, so the grammar excludes everything a path could
/// interpret (`.`, `/`, case). `audiocore::names` defers to this type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct SourceId(String);

impl SourceId {
    pub fn parse(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        let first = chars.next()?;
        let valid = text.len() <= 64
            && (first.is_ascii_lowercase() || first.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        valid.then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

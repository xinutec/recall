//! The kind a source registers in the capture log.

use std::fmt;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SourceKind {
    /// A macOS input device.
    #[serde(rename = "coreaudio")]
    CoreAudio,
    /// Raw s16le PCM over TCP, from the recall-mic app.
    #[serde(rename = "tcp_pcm")]
    TcpPcm,
}

impl SourceKind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "coreaudio" => Self::CoreAudio,
            "tcp_pcm" => Self::TcpPcm,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreAudio => "coreaudio",
            Self::TcpPcm => "tcp_pcm",
        }
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

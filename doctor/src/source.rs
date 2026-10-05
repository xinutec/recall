//! The kind a source registers in the capture log.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    /// A macOS input device.
    CoreAudio,
    /// An ffmpeg synthetic source, for testing.
    Lavfi,
    /// A network stream, such as a phone on the LAN.
    Rtsp,
    /// Raw s16le PCM over TCP, from the recall-mic app.
    TcpPcm,
    /// Clips uploaded over HTTP; not captured here.
    Upload,
    /// Audio found on disk with no registered source.
    Discovered,
    /// A stream built rather than recorded (the retired room stream).
    Derived,
}

impl SourceKind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "coreaudio" => Self::CoreAudio,
            "lavfi" => Self::Lavfi,
            "rtsp" => Self::Rtsp,
            "tcp_pcm" => Self::TcpPcm,
            "upload" => Self::Upload,
            "discovered" => Self::Discovered,
            "derived" => Self::Derived,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreAudio => "coreaudio",
            Self::Lavfi => "lavfi",
            Self::Rtsp => "rtsp",
            Self::TcpPcm => "tcp_pcm",
            Self::Upload => "upload",
            Self::Discovered => "discovered",
            Self::Derived => "derived",
        }
    }

    /// Kinds with a recorder that could stop or lose speech. A discovered
    /// source that is a recorder registers its true kind when its agent
    /// starts.
    pub fn is_device(self) -> bool {
        !matches!(self, Self::Upload | Self::Discovered | Self::Derived)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

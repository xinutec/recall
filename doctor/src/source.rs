//! How a source's PCM stream is produced: the kind a source registers in the
//! capture log.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    /// sox — sample-perfect macOS device capture
    CoreAudio,
    /// ffmpeg synthetic source (testing/calibration)
    Lavfi,
    /// ffmpeg network source (e.g. a phone on the LAN)
    Rtsp,
    /// ffmpeg listens for raw s16le PCM (the recall-mic app)
    TcpPcm,
    /// clips uploaded over HTTP (e.g. phone recorder); not captured
    Upload,
    /// Audio found on disk with no registered source; says nothing about what
    /// wrote it.
    Discovered,
    /// A stream built rather than recorded: the room stream, one settled minute
    /// at a time from whichever microphone won it. Not a device: it has no
    /// recorder, and measuring it would double-count the microphone it carries.
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

    /// Kinds that have a recorder which could stop or lose speech.
    ///
    /// An upload is an imported meeting with no microphone behind it. A
    /// discovered source has no known recorder; if it is one, its agent
    /// registers the true kind on start and it joins this set.
    pub fn is_device(self) -> bool {
        !matches!(self, Self::Upload | Self::Discovered | Self::Derived)
    }
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

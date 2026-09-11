//! How a source's PCM stream is produced — the archive's `sources.kind`.

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
    /// Audio the worker found on disk with no registered source. It is an
    /// admission, not a producer: nothing here says what wrote those files.
    Discovered,
    /// A stream this system BUILT rather than recorded: the room stream, one
    /// settled minute at a time from whichever microphone won it (stage D3).
    ///
    /// Not a device, and the distinction is load-bearing: `deaf`, the liveness
    /// view and the sources panel all ask `is_device()`, and a derived stream
    /// has no recorder to be deaf, no `.alive` marker, and no phone to blame.
    /// It inherits whichever microphone's audio it carried, so measuring it as a
    /// microphone would double-count the one that was already measured.
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
    /// An UPLOAD is a meeting someone imported: real speech we chose to keep,
    /// with no microphone behind it. A DISCOVERED source has no recorder
    /// either, so every device check would be answering a question about a
    /// machine that may not exist — if it really is a recorder, its agent
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

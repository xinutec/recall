//! doctor: is recall working? Agents loaded, archive mirrored and, above all,
//! is the recording recording.
//!
//! The reporting process never reads the archive volume. That happens in
//! [`archive`], in a child process the parent abandons if it hangs
//! ([`bounded`]): launchd starts no new run while one is stuck, so one doctor
//! wedged in disk wait would silence all later ones. The parent reads only the
//! boot disk and, with timeouts, the server ([`live`], [`deaf`]).

pub mod agents;
pub mod archive;
pub mod bounded;
pub mod capture;
pub mod check;
pub mod deaf;
pub mod delivery;
pub mod fleetwatch;
pub mod live;
pub mod loss;
pub mod record;
pub mod source;

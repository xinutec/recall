//! doctor: is recall working? Agents loaded, archive mirrored and, above all,
//! is the recording actually recording.
//!
//! **The reporting process never reads the archive volume.** Everything that
//! does lives in [`archive`], behind a child process the parent abandons if it
//! hangs ([`bounded`]). With `KeepAlive = false` and a 300s `StartInterval`,
//! launchd starts no new run while one is stuck, so one doctor wedged in disk
//! wait would silence every doctor after it. The parent reads launchd and
//! `~/.config` (boot disk) plus bounded network reads of the fleet: the live
//! tier's output ([`live`]) and every microphone's speech ([`deaf`]).

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
pub mod source;

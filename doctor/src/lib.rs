//! doctor — is recall working?
//!
//! Agents loaded, archive mirrored — and, above all, is the recording actually
//! recording. That last one was missing, and its absence cost real memory:
//! capture crash-looped on 22 June, recorded nothing for ninety minutes, and was
//! found three weeks later by diffing the filesystem by hand.
//!
//! The whole crate is shaped by one boundary. **The reporting process must never
//! read the archive volume.** Everything that does lives in [`archive`], behind a
//! child process the parent abandons if it hangs ([`bounded`]). On 2026-08-10 the
//! doctor sat in uninterruptible disk wait for over an hour — with
//! `KeepAlive = false` and a 300s `StartInterval`, launchd starts no further run
//! while one is stuck, so a single wedged doctor silenced every doctor after it.
//! What runs in the parent reads launchd and `~/.config`, both on the boot disk.

pub mod agents;
pub mod archive;
pub mod blanked;
pub mod bounded;
pub mod capture;
pub mod check;
pub mod delivery;
pub mod fleetwatch;
pub mod instant;
pub mod loss;
pub mod source;

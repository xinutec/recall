//! Holding a job's lease while its model runs: a long meeting outlasts one
//! lease, and a lapsed lease hands the job to a second runner.

use crate::client::Client;
use std::sync::mpsc;
use std::time::Duration;

/// How often a running job is renewed: well inside recalld's ten-minute lease.
pub const EVERY: Duration = Duration::from_mins(2);

/// Run `work` while renewing job `id` every `every`. A failed renewal is
/// logged and the work goes on: at worst the lease lapses and the job is offered again.
pub fn while_renewing<T>(client: &Client, id: i64, every: Duration, work: impl FnOnce() -> T) -> T {
    let (stop, stopped) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                match client.renew(id) {
                    Ok(true) => {}
                    // Finished meanwhile, or a server that does not renew.
                    Ok(false) => return,
                    Err(err) => tracing::warn!(id, %err, "lease renewal failed"),
                }
            }
        });
        let out = work();
        drop(stop);
        out
    })
}

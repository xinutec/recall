//! How each background pass is doing, for the doctor: the loops report here
//! and `GET /sync/record/health` reads it ([`crate::record_health`]). A pass
//! that only logs its failure fails unseen; one did for three hours.

use audiocore::record_health::{Failing, Pass};
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Every pass of this process.
pub static PASSES: Passes = Passes::new();

#[derive(Default)]
pub struct Passes {
    by_name: Mutex<BTreeMap<&'static str, State>>,
}

struct State {
    every: Duration,
    since: DateTime<Utc>,
    off: Option<String>,
    running_since: Option<DateTime<Utc>>,
    last_end: Option<DateTime<Utc>>,
    failing: Option<(DateTime<Utc>, String)>,
}

impl Passes {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_name: Mutex::new(BTreeMap::new()),
        }
    }

    /// A pass that will run every `every`, from `now`.
    pub fn register(&self, name: &'static str, every: Duration, now: DateTime<Utc>) {
        self.with(|m| {
            m.insert(
                name,
                State {
                    every,
                    since: now,
                    off: None,
                    running_since: None,
                    last_end: None,
                    failing: None,
                },
            );
        });
    }

    /// A pass this process will not run, and why.
    pub fn off(&self, name: &'static str, every: Duration, now: DateTime<Utc>, why: String) {
        self.register(name, every, now);
        self.update(name, |s| s.off = Some(why));
    }

    pub fn begin(&self, name: &'static str, now: DateTime<Utc>) {
        self.update(name, |s| s.running_since = Some(now));
    }

    /// A pass ended: clean with `None`, or with what went wrong, which keeps
    /// the pass failing from the first such end until a clean one.
    pub fn ended(&self, name: &'static str, now: DateTime<Utc>, problem: Option<String>) {
        self.update(name, |s| {
            s.running_since = None;
            s.last_end = Some(now);
            s.failing = problem.map(|error| {
                let since = s.failing.as_ref().map_or(now, |(since, _)| *since);
                (since, error)
            });
        });
    }

    /// One run of pass `name`: `job` off the async threads, its end reported
    /// here and a failure or panic logged. `problem` names what a run that
    /// finished still got wrong. `None` when it did not finish.
    pub async fn run<T, E>(
        &self,
        name: &'static str,
        job: impl FnOnce() -> Result<T, E> + Send + 'static,
        problem: impl FnOnce(&T) -> Option<String>,
    ) -> Option<T>
    where
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        self.begin(name, Utc::now());
        let (done, error) = match tokio::task::spawn_blocking(job).await {
            Ok(Ok(done)) => {
                let error = problem(&done);
                (Some(done), error)
            }
            Ok(Err(err)) => {
                tracing::warn!(pass = name, %err, "pass failed");
                (None, Some(err.to_string()))
            }
            Err(err) => {
                tracing::error!(pass = name, %err, "pass panicked");
                (None, Some(err.to_string()))
            }
        };
        self.ended(name, Utc::now(), error);
        done
    }

    #[must_use]
    pub fn report(&self) -> Vec<Pass> {
        let stamp = |t: DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
        self.with(|m| {
            m.iter()
                .map(|(name, s)| Pass {
                    name: (*name).to_owned(),
                    every_s: s.every.as_secs(),
                    since_utc: stamp(s.since),
                    off: s.off.clone(),
                    running_since_utc: s.running_since.map(stamp),
                    last_end_utc: s.last_end.map(stamp),
                    failing: s.failing.as_ref().map(|(since, error)| Failing {
                        since_utc: stamp(*since),
                        error: error.clone(),
                    }),
                })
                .collect()
        })
    }

    fn update(&self, name: &'static str, f: impl FnOnce(&mut State)) {
        self.with(|m| {
            if let Some(state) = m.get_mut(name) {
                f(state);
            }
        });
    }

    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<&'static str, State>) -> T) -> T {
        // A panic elsewhere while holding it leaves the map whole: each
        // change is one assignment.
        f(&mut self.by_name.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

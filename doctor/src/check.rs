//! One fleetwatch check, and the ordering that rolls several into one verdict.

use serde::{Deserialize, Serialize};

/// One fleetwatch check (fleetwatch README, "The report contract").
///
/// `label` is the trend identity and must stay stable across runs; anything
/// that varies per run belongs in `observed` or `value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub section: String,
    pub label: String,
    pub verdict: Verdict,
    pub observed: String,
    pub expected: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Warn,
    Fail,
    /// Not a fault: the thing checked was deliberately switched off.
    Skip,
}

impl Verdict {
    /// Worst-wins, for a roll-up. `Skip` ranks with `Pass`: a deliberate pause
    /// is not a fault.
    fn severity(self) -> u8 {
        match self {
            Verdict::Pass | Verdict::Skip => 0,
            Verdict::Warn => 1,
            Verdict::Fail => 2,
        }
    }

    /// How the terminal prints it.
    pub fn mark(self) -> &'static str {
        match self {
            Verdict::Pass => "ok",
            Verdict::Warn => "WARN",
            Verdict::Fail => "FAIL",
            Verdict::Skip => "--",
        }
    }
}

/// The worst verdict among `checks`, or `Pass` for none.
///
/// Ties keep the first, so this is a fold and not `max_by_key` (which returns
/// the last maximum and would turn `[Pass, Skip]` into `Skip`).
pub fn worst(checks: impl IntoIterator<Item = Verdict>) -> Verdict {
    checks.into_iter().fold(Verdict::Pass, |best, next| {
        if next.severity() > best.severity() {
            next
        } else {
            best
        }
    })
}

/// Builds a [`Check`]; most carry a trendable number via [`Builder::trend`].
pub struct Builder {
    section: &'static str,
    label: String,
    verdict: Verdict,
    observed: String,
    expected: String,
    value: Option<f64>,
    unit: Option<&'static str>,
}

impl Builder {
    pub fn new(
        section: &'static str,
        label: impl Into<String>,
        verdict: Verdict,
        observed: impl Into<String>,
        expected: impl Into<String>,
    ) -> Self {
        Self {
            section,
            label: label.into(),
            verdict,
            observed: observed.into(),
            expected: expected.into(),
            value: None,
            unit: None,
        }
    }

    #[must_use]
    pub fn trend(mut self, value: f64, unit: &'static str) -> Self {
        self.value = Some(value);
        self.unit = Some(unit);
        self
    }

    pub fn build(self) -> Check {
        Check {
            section: self.section.to_owned(),
            label: self.label,
            verdict: self.verdict,
            observed: self.observed,
            expected: self.expected,
            value: self.value,
            unit: self.unit.map(ToOwned::to_owned),
        }
    }
}

/// `check("capture", "recording", Pass, observed, expected).build()`
pub fn check(
    section: &'static str,
    label: impl Into<String>,
    verdict: Verdict,
    observed: impl Into<String>,
    expected: impl Into<String>,
) -> Builder {
    Builder::new(section, label, verdict, observed, expected)
}

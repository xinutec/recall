//! One fleetwatch check, and the ordering that rolls several into one verdict.

use serde::{Deserialize, Serialize};

/// One fleetwatch check — see its report contract (fleetwatch README, "The
/// report contract").
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
    /// Worst-wins, for a roll-up. ⚠ `Skip` ranks WITH `Pass`: a deliberate
    /// pause is not a fault and must never drag a summary upward.
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
/// ⚠ Ties keep the FIRST, which is why this is a fold and not `max_by_key`:
/// that returns the LAST maximum, so a roll-up over `[Pass, Skip]` — equally
/// severe, both zero — came back `Skip`, and a check that passed then rendered
/// as "not applicable".
pub fn worst(checks: impl IntoIterator<Item = Verdict>) -> Verdict {
    checks.into_iter().fold(Verdict::Pass, |best, next| {
        if next.severity() > best.severity() {
            next
        } else {
            best
        }
    })
}

/// A builder that keeps the call sites readable — every check names its
/// section, label, verdict, what was observed and what was expected, and most
/// carry a trendable number.
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

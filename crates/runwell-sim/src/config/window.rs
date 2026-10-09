//! Fit and report windows, for holdout validation on one trace.
use super::Config;
use crate::Error;
use jiff::Timestamp;

fn within(t: Timestamp, since: Option<Timestamp>, until: Option<Timestamp>) -> bool {
    since.is_none_or(|s| t >= s) && until.is_none_or(|u| t < u)
}

impl Config {
    /// Whether a job starting at `started` may fit model parameters.
    pub(crate) fn in_fit_window(&self, started: Option<Timestamp>) -> bool {
        match started {
            Some(t) => within(t, self.fit_since, self.fit_until),
            None => self.fit_since.is_none() && self.fit_until.is_none(),
        }
    }
    /// Whether a run created at `created` belongs to the reported cohorts.
    pub(crate) fn in_report_window(&self, created: Timestamp) -> bool {
        within(created, self.report_since, self.report_until)
    }
    pub(super) fn validate_windows(&self) -> Result<(), Error> {
        let empty = |since: Option<Timestamp>, until: Option<Timestamp>| {
            since.zip(until).is_some_and(|(s, u)| s >= u)
        };
        if empty(self.fit_since, self.fit_until)
            || empty(self.report_since, self.report_until)
            || self.calibration_hosts == 0
            || self.calibration_hosts > self.hosts.len()
        {
            return Err(Error::Invalid(
                "windows must be non-empty and calibration_hosts a configured host count".into(),
            ));
        }
        Ok(())
    }
}

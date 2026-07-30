//! Scheduled pulls.
//!
//! One task per integration, sleeping until its cron's next occurrence *in the
//! user's timezone*. That last part is why the timezone is carried around
//! rather than assumed: a job that should run at 6am local must move when the
//! user travels or when DST shifts, which is what `reschedule_for_user` does.

use chrono::Utc;
use chrono_tz::Tz;
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::defs::Definitions;
use crate::fetch::FetchService;
use crate::model::{PullSource, UserIntegration};
use crate::store::Store;

/// How long after a failed fetch to try once more.
///
/// Providers have brief outages, and waiting a whole cron period would lose the
/// window a daily job covers.
const RETRY_AFTER: Duration = Duration::from_secs(10);

pub struct Scheduler {
    store: Store,
    definitions: Arc<Definitions>,
    fetch: FetchService,
    jobs: Mutex<HashMap<String, JoinHandle<()>>>,
}

impl Scheduler {
    pub fn new(store: Store, definitions: Arc<Definitions>, fetch: FetchService) -> Arc<Self> {
        Arc::new(Self {
            store,
            definitions,
            fetch,
            jobs: Mutex::new(HashMap::new()),
        })
    }

    /// Schedules every existing integration at startup.
    ///
    /// A user whose timezone cannot be read is skipped rather than scheduled
    /// against the wrong day.
    pub async fn load(self: &Arc<Self>) -> anyhow::Result<()> {
        let integrations = self.store.all_integrations().await?;

        for integration in integrations {
            let Some(source) = self
                .definitions
                .pull_source(&integration.integration_type, &integration.entity_type)
            else {
                continue;
            };

            match self.fetch.timezone_for(&integration.user_id).await {
                Ok(timezone) => self.schedule(integration, source.clone(), timezone),
                Err(err) => {
                    tracing::error!(
                        integration = %integration.id,
                        error = ?err,
                        "could not resolve the user's timezone; not scheduling"
                    );
                }
            }
        }

        tracing::info!(
            jobs = self.jobs.lock().expect("jobs lock").len(),
            "scheduled pull jobs"
        );
        Ok(())
    }

    /// Starts (or restarts) the job for one integration.
    pub fn schedule(
        self: &Arc<Self>,
        integration: UserIntegration,
        source: PullSource,
        timezone: Tz,
    ) {
        if source.cron.is_empty() {
            return;
        }

        let schedule = match Schedule::from_str(&normalise_cron(&source.cron)) {
            Ok(schedule) => schedule,
            Err(err) => {
                tracing::error!(
                    integration = %integration.id,
                    cron = %source.cron,
                    error = %err,
                    "invalid cron expression; this integration will never run"
                );
                return;
            }
        };

        let id = integration.id.clone();
        tracing::info!(
            integration = %id,
            provider = %integration.integration_type,
            cron = %source.cron,
            timezone = %timezone,
            "scheduling pull"
        );

        let scheduler = Arc::clone(self);
        let job_id = id.clone();
        let handle = tokio::spawn(async move {
            let id = job_id;
            loop {
                let now = Utc::now().with_timezone(&timezone);
                let Some(next) = schedule.after(&now).next() else {
                    tracing::warn!(integration = %id, "cron has no further occurrences");
                    return;
                };

                let Ok(delay) = (next - now).to_std() else {
                    // The next occurrence is already behind us; yield rather
                    // than spin.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };

                tokio::time::sleep(delay).await;
                scheduler.run_once(&id, &source).await;
            }
        });

        if let Some(previous) = self
            .jobs
            .lock()
            .expect("jobs lock")
            .insert(id.clone(), handle)
        {
            previous.abort();
        }
    }

    /// One firing: jitter, re-read, fetch, and retry once on failure.
    async fn run_once(&self, integration_id: &str, source: &PullSource) {
        if source.jitter > 0 {
            // Spread every user's job for this provider out, rather than
            // arriving in one burst on the hour.
            let minutes = perfice_common::random::below(source.jitter as u64);
            tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
        }

        // Re-read rather than using the captured copy: the user may have
        // changed the field mapping, or deleted the integration during the
        // jitter window.
        let integration = match self.store.integration_by_id(integration_id).await {
            Ok(Some(integration)) => integration,
            Ok(None) => {
                tracing::debug!(integration = %integration_id, "integration is gone; skipping");
                return;
            }
            Err(err) => {
                tracing::error!(integration = %integration_id, error = ?err, "could not load integration");
                return;
            }
        };

        if let Err(err) = self.fetch.pull(&integration, source).await {
            tracing::error!(integration = %integration_id, error = ?err, "pull failed");

            tokio::time::sleep(RETRY_AFTER).await;
            if let Err(err) = self.fetch.pull(&integration, source).await {
                tracing::error!(integration = %integration_id, error = ?err, "retry failed");
            }
        }
    }

    pub fn unschedule(&self, integration_id: &str) {
        if let Some(handle) = self.jobs.lock().expect("jobs lock").remove(integration_id) {
            handle.abort();
            tracing::info!(integration = %integration_id, "unscheduled pull");
        }
    }

    /// Moves a user's jobs to a new timezone.
    pub async fn reschedule_for_user(self: &Arc<Self>, user_id: &str, timezone: Tz) {
        let integrations = match self.store.integrations_by_user(user_id).await {
            Ok(integrations) => integrations,
            Err(err) => {
                tracing::error!(user = %user_id, error = ?err, "could not list integrations");
                return;
            }
        };

        for integration in integrations {
            let Some(source) = self
                .definitions
                .pull_source(&integration.integration_type, &integration.entity_type)
            else {
                continue;
            };

            // Only jobs that were actually running are moved; one that failed
            // to schedule should not be resurrected by a timezone change.
            if !self
                .jobs
                .lock()
                .expect("jobs lock")
                .contains_key(&integration.id)
            {
                continue;
            }

            self.unschedule(&integration.id);
            self.schedule(integration, source.clone(), timezone);
        }
    }
}

/// Accepts both the five-field and six-field cron forms.
///
/// Definitions are written with a leading seconds field, but the five-field
/// form is what most people mean by "cron" and rejecting it would silently
/// leave an integration that never runs.
fn normalise_cron(expression: &str) -> String {
    match expression.split_whitespace().count() {
        5 => format!("0 {expression}"),
        _ => expression.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_a_six_field_cron_alone() {
        assert_eq!(normalise_cron("* * * * * *"), "* * * * * *");
    }

    #[test]
    fn gives_a_five_field_cron_a_seconds_field() {
        assert_eq!(normalise_cron("30 6 * * *"), "0 30 6 * * *");
    }

    #[test]
    fn both_forms_parse() {
        assert!(Schedule::from_str(&normalise_cron("* * * * * *")).is_ok());
        assert!(Schedule::from_str(&normalise_cron("30 6 * * *")).is_ok());
    }

    #[test]
    fn a_seconds_cron_fires_within_the_second() {
        let schedule = Schedule::from_str(&normalise_cron("* * * * * *")).unwrap();
        let now = Utc::now().with_timezone(&Tz::UTC);
        let next = schedule.after(&now).next().unwrap();
        assert!((next - now).num_milliseconds() <= 1000);
    }
}

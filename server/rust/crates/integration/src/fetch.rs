//! Talking to providers.
//!
//! Everything a fetch needs that is not the payload itself lives here: which
//! timezone to evaluate the user's dates in, which URL the definition resolves
//! to, and what credential to present.

use anyhow::{Context, bail};
use chrono::Duration;
use chrono_tz::Tz;
use perfice_proto::GetUserTimeZoneRequest;
use perfice_proto::user_service_client::UserServiceClient;
use std::sync::Arc;
use tonic::transport::Channel;

use crate::auth::AuthService;
use crate::defs::Definitions;
use crate::model::{PullSource, UserIntegration};
use crate::paths::{self, Instants};
use crate::process::{ProcessError, Processor};

/// How far back a historical backfill reaches.
const HISTORICAL_DAYS: i64 = 15;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone)]
pub struct FetchService {
    definitions: Arc<Definitions>,
    auth: Arc<AuthService>,
    processor: Processor,
    users: UserServiceClient<Channel>,
    http: reqwest::Client,
}

impl FetchService {
    pub fn new(
        definitions: Arc<Definitions>,
        auth: Arc<AuthService>,
        processor: Processor,
        users: UserServiceClient<Channel>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            definitions,
            auth,
            processor,
            users,
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .context("failed to build the provider http client")?,
        })
    }

    /// The timezone the user's dates are evaluated in.
    ///
    /// Everything date-shaped in a definition -- `[DATE]`, a `$date`
    /// aggregator, the cron itself -- means "in the user's day", so this is not
    /// cosmetic: getting it wrong files records under the wrong date.
    pub async fn timezone_for(&self, user_id: &str) -> anyhow::Result<Tz> {
        let response = self
            .users
            .clone()
            .get_user_time_zone(GetUserTimeZoneRequest {
                user_id: user_id.to_owned(),
            })
            .await
            .context("failed to look up the user's timezone")?
            .into_inner();

        Ok(parse_timezone(&response.timezone))
    }

    /// Runs one scheduled pull.
    pub async fn pull(
        &self,
        integration: &UserIntegration,
        source: &PullSource,
    ) -> Result<(), ProcessError> {
        let timezone = self.timezone_for(&integration.user_id).await?;
        let at = Instants::at(chrono::Utc::now().with_timezone(&timezone));

        let Some(definition) = self
            .definitions
            .entity(&integration.integration_type, &integration.entity_type)
        else {
            return Ok(());
        };

        let options = paths::option_values(&definition.options, &integration.options);
        let url = paths::replace_variables(&source.url, &options, &at);

        let Some(body) = self.request(integration, &url).await? else {
            return Ok(());
        };

        self.processor
            .handle_response(definition, integration, &body, &at)
            .await
    }

    /// Backfills the last two weeks, on the user's explicit request.
    pub async fn historical(&self, integration: &UserIntegration) -> Result<(), ProcessError> {
        let timezone = self.timezone_for(&integration.user_id).await?;
        let now = chrono::Utc::now().with_timezone(&timezone);
        let at = Instants::range(now, now - Duration::days(HISTORICAL_DAYS), now);

        let Some(definition) = self
            .definitions
            .entity(&integration.integration_type, &integration.entity_type)
        else {
            return Ok(());
        };

        let Some(history) = &definition.history else {
            return Err(anyhow::anyhow!("this integration does not support history").into());
        };

        let options = paths::option_values(&definition.options, &integration.options);
        // `[START]` and `[END]` are what make this a *range* request rather
        // than a repeat of the routine pull.
        let url = paths::replace_variables(&history.url, &options, &at);

        let Some(body) = self.request(integration, &url).await? else {
            return Ok(());
        };

        self.processor
            .handle_response(definition, integration, &body, &at)
            .await
    }

    /// Fetches a URL as the user.
    ///
    /// `None` means there was no usable credential -- the user has not
    /// connected this provider, or renewing their token failed. That is an
    /// ordinary state, not a fault, and must not be reported as one.
    async fn request(
        &self,
        integration: &UserIntegration,
        url: &str,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(definition) = self
            .definitions
            .integration_type(&integration.integration_type)
        else {
            bail!("unknown provider {}", integration.integration_type);
        };

        let mut request = self.http.get(url);

        if definition.authentication.is_some() {
            let token = self
                .auth
                .access_token(&integration.user_id, &integration.integration_type)
                .await?;

            let Some(token) = token else {
                tracing::info!(
                    user = %integration.user_id,
                    provider = %integration.integration_type,
                    "no usable credentials; skipping fetch"
                );
                return Ok(None);
            };

            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("could not reach {url}"))?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("failed to read the response")?;

        if !status.is_success() {
            bail!("provider answered {status}");
        }

        Ok(Some(body.to_vec()))
    }
}

/// Resolves an IANA timezone name, falling back to UTC.
///
/// A user whose timezone cannot be read still gets their data; it is filed
/// against UTC days rather than nothing at all.
pub fn parse_timezone(name: &str) -> Tz {
    match name.parse::<Tz>() {
        Ok(timezone) => timezone,
        Err(_) => {
            tracing::warn!(timezone = %name, "unknown timezone; falling back to UTC");
            Tz::UTC
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_timezone;
    use chrono_tz::Tz;

    #[test]
    fn resolves_a_known_zone() {
        assert_eq!(parse_timezone("Europe/Amsterdam"), Tz::Europe__Amsterdam);
    }

    #[test]
    fn falls_back_to_utc() {
        assert_eq!(parse_timezone(""), Tz::UTC);
        assert_eq!(parse_timezone("Mars/Olympus"), Tz::UTC);
    }
}

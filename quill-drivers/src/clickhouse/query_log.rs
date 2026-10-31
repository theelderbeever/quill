//! Per-execution server metrics from `system.query_log`.
use quill_core::{
    error::Result,
    query::{QueryId, ServerMetrics, ServerMetricsError},
};
use std::time::Duration;

use super::ClickhouseConnectionPool;

/// Where execution log rows live. Replica-wide reads find the initiator row even
/// when a load balancer routed the metrics lookup to a different replica.
#[derive(Debug, Clone, Copy)]
pub(super) enum QueryLog {
    AllReplicas,
    Local,
}

impl QueryLog {
    /// Choose the log once at connect; this also serves as the connection check.
    /// Errors other than a missing cluster or remote-read grant still fail the connect.
    pub(super) async fn probe(client: &clickhouse::Client) -> clickhouse::error::Result<Self> {
        let probe = client
            .query("SELECT 1 FROM clusterAllReplicas('{cluster}', system.one) LIMIT 1")
            .execute()
            .await;
        match probe {
            Ok(()) => Ok(Self::AllReplicas),
            Err(error)
                if ExceptionCode::of(&error).is_some_and(ExceptionCode::requires_local_log) =>
            {
                Ok(Self::Local)
            }
            Err(error) => Err(error),
        }
    }

    const fn source(self) -> &'static str {
        match self {
            Self::AllReplicas => "clusterAllReplicas('{cluster}', system.query_log)",
            Self::Local => "system.query_log",
        }
    }
}

/// A ClickHouse server exception code, parsed from an HTTP error response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExceptionCode(u32);

impl ExceptionCode {
    /// Raised when the `{cluster}` macro is not configured.
    const BAD_ARGUMENTS: Self = Self(36);
    /// Raised for an unknown cluster before `CLUSTER_DOESNT_EXIST` was introduced.
    const BAD_GET: Self = Self(170);
    const ACCESS_DENIED: Self = Self(497);
    const CLUSTER_DOESNT_EXIST: Self = Self(701);

    fn of(error: &clickhouse::error::Error) -> Option<Self> {
        let clickhouse::error::Error::BadResponse(message) = error else {
            return None;
        };
        let (_, rest) = message.split_once("Code: ")?;
        let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
        digits.parse().ok().map(Self)
    }

    /// Whether a replica-wide read failed only because this deployment has no
    /// usable `{cluster}` macro, or this user may not read remote replicas.
    fn requires_local_log(self) -> bool {
        matches!(
            self,
            Self::BAD_ARGUMENTS | Self::BAD_GET | Self::CLUSTER_DOESNT_EXIST | Self::ACCESS_DENIED
        )
    }
}

#[derive(serde::Deserialize, clickhouse::Row)]
struct MetricsRow {
    query_duration_ms: u64,
    read_rows: u64,
    read_bytes: u64,
    written_rows: u64,
    written_bytes: u64,
    result_rows: u64,
    result_bytes: u64,
    memory_usage: u64,
}

impl From<MetricsRow> for ServerMetrics {
    fn from(row: MetricsRow) -> Self {
        Self {
            elapsed: Some(Duration::from_millis(row.query_duration_ms)),
            read_rows: Some(row.read_rows),
            read_bytes: Some(row.read_bytes),
            written_rows: Some(row.written_rows),
            written_bytes: Some(row.written_bytes),
            result_rows: Some(row.result_rows),
            result_bytes: Some(row.result_bytes),
            peak_memory_bytes: Some(row.memory_usage),
            ..Self::unknown()
        }
    }
}

impl ClickhouseConnectionPool {
    /// The `query_id` setting that correlates an execution with its log rows.
    pub(super) fn server_query_id(id: QueryId) -> String {
        format!("quill::{id}")
    }

    pub(super) async fn logged_metrics(&self, id: QueryId) -> Result<Option<ServerMetrics>> {
        self.fetch_logged(self.query_log, &Self::server_query_id(id))
            .await
            .map_err(classify)
    }

    async fn fetch_logged(
        &self,
        log: QueryLog,
        query_id: &str,
    ) -> clickhouse::error::Result<Option<ServerMetrics>> {
        // `is_initial_query` selects the initiator row, whose read counters include remote work.
        let sql = format!(
            "SELECT query_duration_ms, read_rows, read_bytes, written_rows, written_bytes, \
             result_rows, result_bytes, toUInt64(greatest(memory_usage, 0)) AS memory_usage \
             FROM {} WHERE query_id = ? AND is_initial_query \
             AND type IN ('QueryFinish', 'ExceptionWhileProcessing') \
             ORDER BY event_time_microseconds DESC LIMIT 1",
            log.source()
        );
        let row = self
            .client
            .query(&sql)
            .bind(query_id)
            .fetch_optional::<MetricsRow>()
            .await?;
        Ok(row.map(ServerMetrics::from))
    }
}

fn classify(error: clickhouse::error::Error) -> quill_core::error::Error {
    let source = Box::new(error);
    if ExceptionCode::of(&source) == Some(ExceptionCode::ACCESS_DENIED) {
        ServerMetricsError::PermissionDenied(source).into()
    } else {
        ServerMetricsError::Unavailable(source).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exception_codes_from_responses() {
        let denied = clickhouse::error::Error::BadResponse(
            "Code: 497. DB::Exception: quill_restricted: Not enough privileges. (ACCESS_DENIED)"
                .into(),
        );
        assert_eq!(
            ExceptionCode::of(&denied),
            Some(ExceptionCode::ACCESS_DENIED)
        );
        let bare = clickhouse::error::Error::BadResponse("Code: 701".into());
        assert_eq!(
            ExceptionCode::of(&bare),
            Some(ExceptionCode::CLUSTER_DOESNT_EXIST)
        );
        assert_eq!(ExceptionCode::of(&clickhouse::error::Error::TimedOut), None);
        let unrelated = clickhouse::error::Error::BadResponse("bad gateway".into());
        assert_eq!(ExceptionCode::of(&unrelated), None);
    }
}

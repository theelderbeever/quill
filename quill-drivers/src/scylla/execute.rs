use super::{result::Kind, *};
use arrow::{datatypes::Schema, record_batch::RecordBatch};
use futures::TryStreamExt;
use quill_core::query::*;
use scylla::{
    errors::{RequestAttemptError, RequestError},
    observability::history::{AttemptId, HistoryListener, RequestId, SpeculativeId},
    policies::retry::RetryDecision,
    value::{CqlValue, Row},
};
use std::sync::Arc;
/// Records each request, speculative fiber, and attempt into live client
/// metrics, so retries are visible while a query is still paging.
struct HistoryCounter(Arc<crate::execution::Control>);

impl HistoryCounter {
    fn update(&self, update: impl FnOnce(&mut DriverHistory) -> usize) -> usize {
        let mut id = 0;
        self.0.record(|metrics| {
            let history = metrics.driver.get_or_insert_with(|| DriverHistory {
                requests: 0,
                attempts: 0,
                speculative_executions: 0,
                nodes: Vec::new(),
            });
            id = update(history);
        });
        id
    }
}

impl std::fmt::Debug for HistoryCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryCounter")
    }
}

impl HistoryListener for HistoryCounter {
    fn log_request_start(&self) -> RequestId {
        RequestId(self.update(|history| {
            history.requests += 1;
            history.requests as usize
        }))
    }
    fn log_request_success(&self, _: RequestId) {}
    fn log_request_error(&self, _: RequestId, _: &RequestError) {}
    fn log_new_speculative_fiber(&self, _: RequestId) -> SpeculativeId {
        SpeculativeId(self.update(|history| {
            history.speculative_executions += 1;
            history.speculative_executions as usize
        }))
    }
    fn log_attempt_start(
        &self,
        _: RequestId,
        _: Option<SpeculativeId>,
        node: std::net::SocketAddr,
    ) -> AttemptId {
        AttemptId(self.update(|history| {
            history.attempts += 1;
            if !history.nodes.contains(&node) {
                history.nodes.push(node);
            }
            history.attempts as usize
        }))
    }
    fn log_attempt_success(&self, _: AttemptId) {}
    fn log_attempt_error(&self, _: AttemptId, _: &RequestAttemptError, _: &RetryDecision) {}
}

impl ScyllaConnectionPool {
    pub(super) async fn run(
        session: Arc<Session>,
        request: QueryRequest,
        sender: tokio::sync::mpsc::Sender<Result<QueryEvent>>,
        control: Arc<crate::execution::Control>,
    ) -> Result<()> {
        let mut statement = session
            .prepare(request.sql)
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        statement.set_history_listener(Arc::new(HistoryCounter(control.clone())));
        let named = request
            .bindings
            .named
            .into_iter()
            .map(|(name, value)| Ok((name, Self::parameter(value)?)))
            .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
        let positional = request
            .bindings
            .positional
            .into_iter()
            .map(Self::parameter)
            .collect::<Result<Vec<_>>>()?;
        let values = Parameters { named, positional };
        let pager = session
            .execute_iter(statement, values)
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let kinds = pager
            .column_specs()
            .iter()
            .map(|s| Kind::parse(s.typ()))
            .collect::<Result<Vec<_>>>()?;
        let schema = Arc::new(Schema::new(
            pager
                .column_specs()
                .iter()
                .zip(&kinds)
                .map(|(s, k)| k.field(s.name()))
                .collect::<Vec<_>>(),
        ));
        if !kinds.is_empty() {
            control
                .send(
                    &sender,
                    QueryEvent::Schema {
                        result_set: 0,
                        schema: schema.clone(),
                    },
                )
                .await?;
        }
        let mut stream = pager
            .rows_stream::<Row>()
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        let mut rows = Vec::with_capacity(1024);
        while let Some(row) = stream
            .try_next()
            .await
            .map_err(|e| Error::QueryExecution(Box::new(e)))?
        {
            rows.push(row);
            if rows.len() == 1024 {
                Self::batch(&rows, &kinds, &schema, &sender, &control).await?;
                rows.clear();
            }
        }
        if !rows.is_empty() {
            Self::batch(&rows, &kinds, &schema, &sender, &control).await?;
        }
        control
            .send(
                &sender,
                QueryEvent::ResultSetComplete {
                    result_set: 0,
                    affected_rows: None,
                },
            )
            .await
    }
    async fn batch(
        rows: &[Row],
        kinds: &[Kind],
        schema: &Arc<Schema>,
        sender: &tokio::sync::mpsc::Sender<Result<QueryEvent>>,
        control: &crate::execution::Control,
    ) -> Result<()> {
        let arrays = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| k.array(rows, i))
            .collect::<Result<Vec<_>>>()?;
        let batch = RecordBatch::try_new(schema.clone(), arrays)
            .map_err(|e| Error::QueryExecution(Box::new(e)))?;
        control
            .send(
                sender,
                QueryEvent::Batch {
                    result_set: 0,
                    batch,
                },
            )
            .await
    }
}
impl ScyllaConnectionPool {
    fn parameter(value: ParameterValue) -> Result<Option<CqlValue>> {
        use chrono::Timelike;
        use scylla::value::{CqlDate, CqlTime, CqlTimestamp};
        Ok(Some(match value {
            ParameterValue::Null => return Ok(None),
            ParameterValue::Boolean(v) => CqlValue::Boolean(v),
            ParameterValue::Int64(v) => CqlValue::BigInt(v),
            ParameterValue::UInt64(v) => {
                CqlValue::BigInt(i64::try_from(v).map_err(|_| {
                    Error::UnsupportedOperation("Scylla unsigned integer exceeds i64")
                })?)
            }
            ParameterValue::Float64(v) => CqlValue::Double(v),
            ParameterValue::Text(v) => CqlValue::Text(v),
            ParameterValue::Binary(v) => CqlValue::Blob(v),
            #[allow(clippy::unwrap_used)]
            ParameterValue::Date(v) => CqlValue::Date(CqlDate(
                ((v - chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()).num_days()
                    + (1i64 << 31)) as u32,
            )),
            ParameterValue::Time(v) => CqlValue::Time(CqlTime(
                i64::from(v.num_seconds_from_midnight()) * 1_000_000_000
                    + i64::from(v.nanosecond()),
            )),
            ParameterValue::Timestamp(v) => {
                CqlValue::Timestamp(CqlTimestamp(v.and_utc().timestamp_millis()))
            }
            ParameterValue::TimestampWithTimezone(v) => {
                CqlValue::Timestamp(CqlTimestamp(v.timestamp_millis()))
            }
            ParameterValue::Uuid(v) => CqlValue::Uuid(v),
        }))
    }
}

// Delegate serialization and binding errors to Scylla while interleaving styles.
struct Parameters {
    named: std::collections::BTreeMap<String, Option<CqlValue>>,
    positional: Vec<Option<CqlValue>>,
}

impl scylla::serialize::row::SerializeRow for Parameters {
    fn serialize(
        &self,
        ctx: &scylla::serialize::row::RowSerializationContext<'_>,
        writer: &mut scylla::serialize::writers::RowWriter,
    ) -> std::result::Result<(), scylla::serialize::SerializationError> {
        use scylla::serialize::row::RowSerializationContext;
        if self.named.is_empty() {
            return self.positional.serialize(ctx, writer);
        }
        if self.positional.is_empty() {
            return self.named.serialize(ctx, writer);
        }
        let mut positional = self.positional.as_slice();
        for column in ctx.columns() {
            let column_ctx = RowSerializationContext::from_specs(std::slice::from_ref(column));
            if let Some(value) = self.named.get(column.name()) {
                std::slice::from_ref(value).serialize(&column_ctx, writer)?;
            } else {
                let (value, remaining) = positional.split_at(positional.len().min(1));
                value.serialize(&column_ctx, writer)?;
                positional = remaining;
            }
        }
        positional.serialize(&RowSerializationContext::empty(), writer)?;
        let unused = self
            .named
            .iter()
            .filter(|(name, _)| !ctx.columns().iter().any(|c| c.name() == name.as_str()))
            .map(|(name, value)| (name.as_str(), value))
            .collect::<std::collections::BTreeMap<_, _>>();
        unused.serialize(&RowSerializationContext::empty(), writer)
    }
    fn is_empty(&self) -> bool {
        self.named.is_empty() && self.positional.is_empty()
    }
}

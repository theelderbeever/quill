//! Column statistics and server metrics for a finished run.
use std::{cmp::Ordering, sync::Arc, time::Duration};

use quill_core::{query::QueryId, value::Value};
use quill_providers::ConnectionPool;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::{event::Msg, result::ResultTable};

/// Attempts at reading server metrics, which some servers log late.
const METRIC_ATTEMPTS: u32 = 3;
const METRIC_RETRY: Duration = Duration::from_secs(1);

pub struct ColumnProfile {
    pub name: String,
    pub data_type: String,
    pub nulls: u64,
    /// Absent when every value is null, or for values without a useful
    /// order: bytes and lists.
    pub min: Option<String>,
    pub max: Option<String>,
}

pub struct Profile {
    pub rows: u64,
    pub columns: Vec<ColumnProfile>,
}

/// Profile `table` with one pass per column, off the UI thread.
pub fn spawn_profile(table: Arc<ResultTable>, run: RunId, messages: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || profile(&table))
            .await
            .map_err(anyhow::Error::from);
        let _ = messages.send(Msg::Run(RunMsg::Profile { run, result }));
    });
}

fn profile(table: &ResultTable) -> Profile {
    let columns = table
        .columns()
        .iter()
        .enumerate()
        .map(|(i, column)| {
            let mut nulls = 0;
            let mut orderable = true;
            let mut bounds: Option<(&Value, &Value)> = None;
            for value in table.iter().map(|row| &row[i]) {
                match value {
                    Value::Null => nulls += 1,
                    Value::Bytes(_) | Value::List(_) => orderable = false,
                    value => {
                        bounds = Some(match bounds {
                            None => (value, value),
                            Some((lo, hi)) => (
                                if value.sort_cmp(lo) == Ordering::Less {
                                    value
                                } else {
                                    lo
                                },
                                if value.sort_cmp(hi) == Ordering::Greater {
                                    value
                                } else {
                                    hi
                                },
                            ),
                        });
                    }
                }
            }
            let bounds = bounds.filter(|_| orderable);
            ColumnProfile {
                name: column.name.clone(),
                data_type: column.native_type.clone(),
                nulls,
                min: bounds.map(|(lo, _)| lo.to_string()),
                max: bounds.map(|(_, hi)| hi.to_string()),
            }
        })
        .collect();
    Profile {
        rows: table.rows(),
        columns,
    }
}

/// Look up the server's measurements, retrying while the server reports none.
pub fn spawn_server_metrics(
    pool: Arc<dyn ConnectionPool>,
    query: QueryId,
    run: RunId,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let mut result = Ok(None);
        for attempt in 0..METRIC_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(METRIC_RETRY).await;
            }
            result = pool.execution_metrics(query).await;
            if !matches!(result, Ok(None)) {
                break;
            }
        }
        let _ = messages.send(Msg::Run(RunMsg::ServerMetrics { run, result }));
    });
}

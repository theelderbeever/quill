//! Column statistics and server metrics for a finished run.
use std::{sync::Arc, time::Duration};

use arrow::{
    array::AsArray,
    datatypes::{DataType, Int64Type, SchemaRef},
    util::display::{ArrayFormatter, FormatOptions},
};
use datafusion::prelude::SessionContext;
use quill_core::query::QueryId;
use quill_drivers::ConnectionPool;
use tokio::sync::mpsc::UnboundedSender;

use super::{RunId, RunMsg};
use crate::{connection::quote, event::Msg};

/// Attempts at reading server metrics, which some servers log late.
const METRIC_ATTEMPTS: u32 = 3;
const METRIC_RETRY: Duration = Duration::from_secs(1);

pub struct ColumnProfile {
    pub name: String,
    pub data_type: String,
    pub nulls: u64,
    /// Absent for types without an ordering, e.g. lists and structs.
    pub min: Option<String>,
    pub max: Option<String>,
}

pub struct Profile {
    pub rows: u64,
    pub columns: Vec<ColumnProfile>,
}

/// Types `min`/`max` order.
fn orderable(data_type: &DataType) -> bool {
    !data_type.is_nested()
        && !matches!(
            data_type,
            DataType::Null
                | DataType::Binary
                | DataType::LargeBinary
                | DataType::BinaryView
                | DataType::FixedSizeBinary(_)
        )
}

/// Profile `table` in `ctx` with one full scan.
pub fn spawn_profile(
    ctx: SessionContext,
    table: &'static str,
    schema: SchemaRef,
    run: RunId,
    messages: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let result = profile(&ctx, table, &schema).await;
        let _ = messages.send(Msg::Run(RunMsg::Profile { run, result }));
    });
}

async fn profile(ctx: &SessionContext, table: &str, schema: &SchemaRef) -> anyhow::Result<Profile> {
    let mut select = vec!["count(*) AS total".to_owned()];
    for (i, field) in schema.fields().iter().enumerate() {
        let column = quote(field.name(), '"');
        select.push(format!("count({column}) AS c{i}"));
        if orderable(field.data_type()) {
            select.push(format!("min({column}) AS lo{i}, max({column}) AS hi{i}"));
        }
    }
    let sql = format!("SELECT {} FROM {table}", select.join(", "));
    let batches = ctx.sql(&sql).await?.collect().await?;
    let batch = batches
        .first()
        .ok_or_else(|| anyhow::anyhow!("profile returned no rows"))?;
    let count = |name: &str| -> anyhow::Result<u64> {
        let column = batch
            .column_by_name(name)
            .ok_or_else(|| anyhow::anyhow!("missing {name}"))?;
        Ok(u64::try_from(column.as_primitive::<Int64Type>().value(0)).unwrap_or(0))
    };
    let options = FormatOptions::default();
    let value = |name: &str| -> anyhow::Result<Option<String>> {
        let Some(column) = batch.column_by_name(name) else {
            return Ok(None);
        };
        if column.is_null(0) {
            return Ok(None);
        }
        Ok(Some(
            ArrayFormatter::try_new(column.as_ref(), &options)?
                .value(0)
                .to_string(),
        ))
    };
    let rows = count("total")?;
    let columns = schema
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| {
            Ok(ColumnProfile {
                name: field.name().clone(),
                data_type: field.data_type().to_string(),
                nulls: rows - count(&format!("c{i}"))?,
                min: value(&format!("lo{i}"))?,
                max: value(&format!("hi{i}"))?,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Profile { rows, columns })
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

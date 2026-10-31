//! Arrow IPC (Feather v2) result storage.
//!
//! [`FeatherStore`] drains a [`QueryExecution`] into one uncompressed IPC file
//! per result set; [`StoredResult`] pages rows back by reading only the blocks
//! that overlap the requested range.
use std::{
    borrow::Borrow,
    collections::BTreeMap,
    fs::File,
    io::{self, BufWriter, Write},
    num::{NonZeroU64, NonZeroUsize},
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use arrow::{
    buffer::Buffer,
    datatypes::SchemaRef,
    error::ArrowError,
    ipc::{Block, reader::FileDecoder, writer::FileWriter},
    record_batch::RecordBatch,
};
use futures::StreamExt;
use quill_core::query::{ExecutionStatus, QueryEvent, QueryExecution};

use crate::error::{Error, Result};

#[cfg(feature = "datafusion")]
mod table;
#[cfg(feature = "datafusion")]
pub use table::{FeatherTable, ROW_COLUMN};

/// Upper bound on bytes written across all result sets of one query.
///
/// ```
/// use quill_io::feather::ByteLimit;
/// assert!(ByteLimit::new(0).is_none());
/// assert_eq!(ByteLimit::new(1024).map(ByteLimit::get), Some(1024));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteLimit(NonZeroU64);

impl ByteLimit {
    pub const fn new(bytes: u64) -> Option<Self> {
        match NonZeroU64::new(bytes) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }

    pub const fn from_non_zero(bytes: NonZeroU64) -> Self {
        Self(bytes)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Live sink counters, shared with whoever displays progress.
#[derive(Debug, Default)]
pub struct SinkProgress {
    rows_written: AtomicU64,
    bytes_written: AtomicU64,
}

/// A point-in-time read of [`SinkProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkMetrics {
    pub rows_written: u64,
    pub bytes_written: u64,
}

impl SinkProgress {
    pub fn snapshot(&self) -> SinkMetrics {
        SinkMetrics {
            rows_written: self.rows_written.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
        }
    }
}

/// Why a stored query holds fewer rows than the query produced.
#[derive(Debug)]
pub enum Truncation {
    Cancelled,
    ByteLimit,
    Failed(quill_core::error::Error),
}

/// Everything drained from one execution.
#[derive(Debug)]
pub struct StoredQuery {
    /// Ordered by result set index.
    pub result_sets: Vec<StoredResultSet>,
    pub truncation: Option<Truncation>,
}

/// One result set; rowless statements have no stored data.
#[derive(Debug)]
pub struct StoredResultSet {
    pub index: usize,
    pub affected_rows: Option<u64>,
    pub data: Option<StoredResult>,
}

/// Writes query results under a caller-owned directory, one file per result set.
#[derive(Debug, Clone)]
pub struct FeatherStore {
    dir: PathBuf,
    limit: Option<ByteLimit>,
}

impl FeatherStore {
    pub fn new(dir: impl Into<PathBuf>, limit: Option<ByteLimit>) -> Self {
        Self {
            dir: dir.into(),
            limit,
        }
    }

    /// Drain `execution` to disk. Owning the execution means dropping this
    /// future cancels the query. Stream failures, cancellation and the byte
    /// limit are reported as [`Truncation`], keeping rows written so far readable.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use arrow::{array::Int64Array, datatypes::*, record_batch::RecordBatch};
    /// # use quill_core::query::*;
    /// # use quill_io::feather::{FeatherStore, SinkProgress};
    /// # struct Done;
    /// # impl ExecutionControl for Done {
    /// #     fn cancel(&self) {}
    /// #     fn snapshot(&self) -> ExecutionSnapshot {
    /// #         ExecutionSnapshot { status: ExecutionStatus::Completed, metrics: ClientMetrics::started() }
    /// #     }
    /// # }
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
    /// let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1, 2, 3]))])?;
    /// let events = vec![
    ///     Ok(QueryEvent::Schema { result_set: 0, schema }),
    ///     Ok(QueryEvent::Batch { result_set: 0, batch }),
    ///     Ok(QueryEvent::ResultSetComplete { result_set: 0, affected_rows: None }),
    ///     Ok(QueryEvent::Complete),
    /// ];
    /// let execution = QueryExecution {
    ///     id: QueryId::new(),
    ///     events: Box::pin(futures::stream::iter(events)),
    ///     control: Arc::new(Done),
    /// };
    ///
    /// let dir = tempfile::tempdir()?;
    /// let progress = SinkProgress::default();
    /// let stored = FeatherStore::new(dir.path(), None).drain(execution, &progress).await?;
    /// assert!(stored.truncation.is_none());
    /// assert_eq!(stored.result_sets[0].data.as_ref().map(|r| r.rows()), Some(3));
    /// assert_eq!(progress.snapshot().rows_written, 3);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn drain(
        &self,
        mut execution: QueryExecution,
        progress: &SinkProgress,
    ) -> Result<StoredQuery> {
        let mut open: BTreeMap<usize, OpenSet> = BTreeMap::new();
        let mut done = Vec::new();
        let mut written = 0u64;

        let truncation = loop {
            let Some(event) = execution.events.next().await else {
                break match execution.control.snapshot().status {
                    ExecutionStatus::Cancelled => Some(Truncation::Cancelled),
                    _ => None,
                };
            };
            match event {
                Ok(QueryEvent::Schema { result_set, schema }) => {
                    let mut set =
                        OpenSet::create(self.dir.join(format!("{result_set}.arrow")), schema)?;
                    written += set.record(0, progress);
                    open.insert(result_set, set);
                }
                Ok(QueryEvent::Batch { result_set, batch }) => {
                    let set = open
                        .get_mut(&result_set)
                        .ok_or(Error::UnexpectedEvent { result_set })?;
                    let Some(rows) = NonZeroUsize::new(batch.num_rows()) else {
                        continue;
                    };
                    set.writer.write(&batch)?;
                    set.row_ends.push(rows);
                    written += set.record(rows.get() as u64, progress);
                }
                Ok(QueryEvent::ResultSetComplete {
                    result_set,
                    affected_rows,
                }) => {
                    let data = open
                        .remove(&result_set)
                        .map(|set| set.finish(false))
                        .transpose()?;
                    done.push(StoredResultSet {
                        index: result_set,
                        affected_rows,
                        data,
                    });
                }
                Ok(QueryEvent::Complete) => break None,
                Err(e) => break Some(Truncation::Failed(e)),
            }
            if self.limit.is_some_and(|limit| written >= limit.get()) {
                execution.control.cancel();
                break Some(Truncation::ByteLimit);
            }
        };

        for (index, set) in open {
            done.push(StoredResultSet {
                index,
                affected_rows: None,
                data: Some(set.finish(true)?),
            });
        }
        done.sort_by_key(|set| set.index);
        Ok(StoredQuery {
            result_sets: done,
            truncation,
        })
    }
}

/// Counts bytes passed to the inner writer.
struct CountingWriter<W> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A result set whose file is still being written.
struct OpenSet {
    path: PathBuf,
    schema: SchemaRef,
    writer: FileWriter<CountingWriter<BufWriter<File>>>,
    row_ends: RowEnds,
    /// Bytes already reported to progress.
    reported: u64,
}

impl OpenSet {
    fn create(path: PathBuf, schema: SchemaRef) -> Result<Self> {
        let file = File::create(&path).map_err(Error::io(&path))?;
        let inner = CountingWriter {
            inner: BufWriter::new(file),
            count: 0,
        };
        let writer = FileWriter::try_new(inner, &schema)?;
        Ok(Self {
            path,
            schema,
            writer,
            row_ends: RowEnds::new(),
            reported: 0,
        })
    }

    /// Publish newly written rows and bytes; returns the new bytes.
    fn record(&mut self, rows: u64, progress: &SinkProgress) -> u64 {
        let count = self.writer.get_ref().count;
        let bytes = count - self.reported;
        self.reported = count;
        progress.rows_written.fetch_add(rows, Ordering::Relaxed);
        progress.bytes_written.fetch_add(bytes, Ordering::Relaxed);
        bytes
    }

    fn finish(mut self, truncated: bool) -> Result<StoredResult> {
        self.writer.finish()?;
        self.writer
            .into_inner()?
            .inner
            .into_inner()
            .map_err(|e| Error::io(&self.path)(e.into_error()))?;
        StoredResult::open(self.path, self.schema, self.row_ends, truncated)
    }
}

/// Strictly increasing cumulative row totals, one per stored batch.
#[derive(Debug)]
struct RowEnds(Vec<u64>);

impl RowEnds {
    const fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, rows: NonZeroUsize) {
        let end = self.total() + rows.get() as u64;
        self.0.push(end);
    }

    fn total(&self) -> u64 {
        self.0.last().copied().unwrap_or(0)
    }

    /// The batch containing `row` and the row's offset within it.
    fn locate(&self, row: u64) -> Option<(usize, u64)> {
        let idx = self.0.partition_point(|&end| end <= row);
        let end = *self.0.get(idx)?;
        let start = if idx == 0 { 0 } else { self.0[idx - 1] };
        debug_assert!(row < end);
        Some((idx, row - start))
    }
}

/// A finished result set file with random access by row.
#[derive(Debug)]
pub struct StoredResult {
    path: PathBuf,
    schema: SchemaRef,
    row_ends: RowEnds,
    file: File,
    decoder: FileDecoder,
    blocks: Vec<Block>,
    truncated: bool,
}

impl StoredResult {
    fn open(path: PathBuf, schema: SchemaRef, row_ends: RowEnds, truncated: bool) -> Result<Self> {
        let file = File::open(&path).map_err(Error::io(&path))?;
        let len = file.metadata().map_err(Error::io(&path))?.len();

        let mut tail = [0u8; 10];
        let tail_at = len.checked_sub(10).ok_or_else(|| {
            ArrowError::IpcError(format!("{} is too short for an IPC file", path.display()))
        })?;
        read_at(&file, &mut tail, tail_at).map_err(Error::io(&path))?;
        let footer_len = arrow::ipc::reader::read_footer_length(tail)?;
        let mut footer = vec![0u8; footer_len];
        read_at(&file, &mut footer, tail_at - footer_len as u64).map_err(Error::io(&path))?;
        let footer = arrow::ipc::root_as_footer(&footer)
            .map_err(|e| ArrowError::ParseError(format!("invalid IPC footer: {e}")))?;

        let mut decoder = FileDecoder::new(schema.clone(), footer.version());
        for block in footer.dictionaries().iter().flatten() {
            let buf = read_block(&file, block).map_err(Error::io(&path))?;
            decoder.read_dictionary(block, &buf)?;
        }
        let blocks: Vec<Block> = footer.recordBatches().iter().flatten().copied().collect();
        if blocks.len() != row_ends.0.len() {
            return Err(ArrowError::IpcError(format!(
                "{} has {} batches, expected {}",
                path.display(),
                blocks.len(),
                row_ends.0.len()
            ))
            .into());
        }

        Ok(Self {
            path,
            schema,
            row_ends,
            file,
            decoder,
            blocks,
            truncated,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    pub fn rows(&self) -> u64 {
        self.row_ends.total()
    }

    /// The result set did not complete; rows are a prefix of the full result.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Lazily yields batches sliced to `range`, touching only overlapping blocks.
    /// The range is clamped to [`Self::rows`]; `read(0..rows)` is a full scan.
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use arrow::{array::{Array, Int64Array}, datatypes::*, record_batch::RecordBatch};
    /// # use quill_core::query::*;
    /// # use quill_io::feather::{FeatherStore, SinkProgress};
    /// # struct Done;
    /// # impl ExecutionControl for Done {
    /// #     fn cancel(&self) {}
    /// #     fn snapshot(&self) -> ExecutionSnapshot {
    /// #         ExecutionSnapshot { status: ExecutionStatus::Completed, metrics: ClientMetrics::started() }
    /// #     }
    /// # }
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
    /// # let batch = |r: std::ops::Range<i64>| RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from_iter_values(r))]);
    /// # let events = vec![
    /// #     Ok(QueryEvent::Schema { result_set: 0, schema: schema.clone() }),
    /// #     Ok(QueryEvent::Batch { result_set: 0, batch: batch(0..10)? }),
    /// #     Ok(QueryEvent::Batch { result_set: 0, batch: batch(10..20)? }),
    /// #     Ok(QueryEvent::ResultSetComplete { result_set: 0, affected_rows: None }),
    /// #     Ok(QueryEvent::Complete),
    /// # ];
    /// # let execution = QueryExecution { id: QueryId::new(), events: Box::pin(futures::stream::iter(events)), control: Arc::new(Done) };
    /// # let dir = tempfile::tempdir()?;
    /// # let mut stored = FeatherStore::new(dir.path(), None).drain(execution, &SinkProgress::default()).await?;
    /// // Two stored batches of ten rows each.
    /// let result = stored.result_sets.remove(0).data.ok_or("rows")?;
    /// let page: Vec<RecordBatch> = result.read(8..12).collect::<Result<_, _>>()?;
    /// assert_eq!(page.iter().map(|b| b.num_rows()).collect::<Vec<_>>(), [2, 2]);
    /// # Ok(())
    /// # }
    /// ```
    pub fn read(&self, range: Range<u64>) -> Batches<&Self> {
        Batches::new(self, range)
    }

    fn read_batch(&self, idx: usize) -> Result<RecordBatch> {
        let block = &self.blocks[idx];
        let buf = read_block(&self.file, block).map_err(Error::io(&self.path))?;
        self.decoder.read_record_batch(block, &buf)?.ok_or_else(|| {
            ArrowError::IpcError(format!("block {idx} holds no record batch")).into()
        })
    }
}

/// Iterator over a row range of a [`StoredResult`], borrowed or shared.
#[derive(Debug)]
pub struct Batches<S> {
    result: S,
    next: u64,
    end: u64,
}

impl<S: Borrow<StoredResult>> Batches<S> {
    pub(crate) fn new(result: S, range: Range<u64>) -> Self {
        let end = range.end.min(result.borrow().rows());
        Self {
            result,
            next: range.start.min(end),
            end,
        }
    }
}

impl<S: Borrow<StoredResult>> Iterator for Batches<S> {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let result = self.result.borrow();
        let (idx, offset) = result.row_ends.locate(self.next)?;
        match result.read_batch(idx) {
            Ok(batch) => {
                let len = (batch.num_rows() as u64 - offset).min(self.end - self.next);
                self.next += len;
                Some(Ok(batch.slice(offset as usize, len as usize)))
            }
            Err(e) => {
                self.next = self.end;
                Some(Err(e))
            }
        }
    }
}

fn read_block(file: &File, block: &Block) -> io::Result<Buffer> {
    let len = block.metaDataLength() as usize + block.bodyLength() as usize;
    let mut buf = vec![0u8; len];
    read_at(file, &mut buf, block.offset() as u64)?;
    Ok(Buffer::from_vec(buf))
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_finds_batch_and_offset() {
        let mut ends = RowEnds::new();
        for rows in [3, 2, 4] {
            ends.push(NonZeroUsize::new(rows).unwrap_or(NonZeroUsize::MIN));
        }
        assert_eq!(ends.locate(0), Some((0, 0)));
        assert_eq!(ends.locate(3), Some((1, 0)));
        assert_eq!(ends.locate(4), Some((1, 1)));
        assert_eq!(ends.locate(8), Some((2, 3)));
        assert_eq!(ends.locate(9), None);
    }
}

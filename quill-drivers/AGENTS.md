## Query Execution

- Delegate query and parameter validation to the native database driver or database. Do not parse
  SQL or add checks for parameter counts, missing or unused bindings, or other conditions the native
  driver already reports descriptively. Adapt bindings to the native API and propagate its errors.

## DataType Handling

- Preserve native value semantics and precision when converting results to Arrow. RecordBatch values
  are data for downstream computation, not display strings. Do not silently round, truncate, or
  convert exact numbers to floating point.
- Use native precision and scale metadata to select Decimal128 or Decimal256 where representable.
  Establish a stable schema before emitting batches; do not infer a decimal width or scale from a
  sample of values that later batches may exceed.
- Fall back to Utf8 for decimals whose precision and scale are undeclared or unavailable, or whose
  declared type or values cannot be represented exactly by Arrow's decimal types. Preserve their
  exact numeric value in the text representation. Choose the fallback before emitting the schema;
  do not change a field's type between batches.
- Annotate semantic types represented as text using Arrow field metadata. Use `quill.native_type`
  for the database type, `quill.logical_type` for its shared semantic type, and `quill.encoding` for
  its representation. Text-backed decimals use logical type `decimal` and encoding `text`.
  Ordinary text columns must not be labeled as decimals.
- Preserve field metadata in both schema events and RecordBatch schemas. Consumers should be able
  to distinguish a native text value from a typed value carried as text.
- Test Arrow schemas, exact stored values, nulls, and field metadata, including consistency across
  batches. Cover precision boundaries and unsupported values; formatted display output alone does
  not establish conversion correctness.

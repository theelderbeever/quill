use quill_core::metadata::*;

pub(super) struct ColumnField<'a>(pub(super) &'a arrow::datatypes::Field);
impl From<ColumnField<'_>> for ColumnMetadata {
    fn from(ColumnField(field): ColumnField<'_>) -> Self {
        Self {
            name: field.name().clone(),
            native_type: field.data_type().to_string(),
            nullable: Some(field.is_nullable()),
        }
    }
}

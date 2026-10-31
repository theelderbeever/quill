// Decode nullable SQLx values directly into an Arrow array.
macro_rules! primitive {
    ($rows:expr, $index:expr, $ty:ty, $array:ty) => {{
        use sqlx::Row as _;
        let index = $index;
        std::sync::Arc::new(
            ($rows)
                .iter()
                .map(|row| row.try_get::<Option<$ty>, _>(index))
                .collect::<std::result::Result<$array, _>>()?,
        ) as arrow::array::ArrayRef
    }};
}

pub(crate) use primitive;

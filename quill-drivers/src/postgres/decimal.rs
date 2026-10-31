use super::execute::ExecutionError;
type DecimalResult<T> = std::result::Result<T, ExecutionError>;

// Checked, fixed-schema conversion of declared Postgres numerics.
use std::sync::Arc;

use arrow::{
    array::{ArrayRef, Decimal128Array, Decimal256Array},
    datatypes::{DataType, i256},
};
use sqlx::{
    PgConnection, Row,
    postgres::{PgColumn, PgRow},
    types::BigDecimal,
};

/// Only construct after the Postgres declaration fits an Arrow decimal schema.
#[derive(Clone, Copy)]
pub(super) struct DecimalType {
    precision: u8,
    scale: i8,
    wide: bool,
}

impl DecimalType {
    pub(super) async fn for_column(
        column: &PgColumn,
        connection: &mut PgConnection,
    ) -> DecimalResult<Option<Self>> {
        let (Some(relation), Some(attribute)) =
            (column.relation_id(), column.relation_attribute_no())
        else {
            return Ok(None);
        };
        let modifier: i32 = sqlx::query_scalar(
            "SELECT atttypmod FROM pg_catalog.pg_attribute WHERE attrelid = $1 AND attnum = $2",
        )
        .bind(relation)
        .bind(attribute)
        .fetch_one(connection)
        .await?;
        if modifier < 0 {
            return Ok(None);
        }
        Self::from_modifier(modifier).map(Some)
    }

    fn from_modifier(modifier: i32) -> DecimalResult<Self> {
        // PostgreSQL numeric typmods have a four-byte header and signed 11-bit scale.
        const TYPE_MODIFIER_HEADER: i32 = 4;
        if modifier < TYPE_MODIFIER_HEADER {
            return Err(ExecutionError::InvalidNumericModifier(modifier));
        }
        let modifier = modifier - TYPE_MODIFIER_HEADER;
        let precision = (modifier >> 16) & 0xffff;
        let scale = ((modifier & 0x7ff) ^ 0x400) - 0x400;
        // Arrow requires positive scale <= precision, even when PostgreSQL allows otherwise.
        let arrow_precision = u8::try_from(precision.max(scale))
            .map_err(|_| ExecutionError::DecimalRange { precision, scale })?;
        let scale =
            i8::try_from(scale).map_err(|_| ExecutionError::DecimalRange { precision, scale })?;
        let precision = arrow_precision;
        let wide = precision > arrow::datatypes::DECIMAL128_MAX_PRECISION;
        if wide {
            Decimal256Array::from(Vec::<Option<i256>>::new())
                .with_precision_and_scale(precision, scale)?;
        } else {
            Decimal128Array::from(Vec::<Option<i128>>::new())
                .with_precision_and_scale(precision, scale)?;
        }
        Ok(Self {
            precision,
            scale,
            wide,
        })
    }

    pub(super) fn data_type(self) -> DataType {
        if self.wide {
            DataType::Decimal256(self.precision, self.scale)
        } else {
            DataType::Decimal128(self.precision, self.scale)
        }
    }

    pub(super) fn array(self, rows: &[PgRow], index: usize) -> DecimalResult<ArrayRef> {
        if self.wide {
            let values = rows
                .iter()
                .map(|row| {
                    row.try_get::<Option<BigDecimal>, _>(index)?
                        .map(|value| {
                            let value = self.exact_scale(value)?;
                            let (coefficient, _) = value.as_bigint_and_exponent();
                            let bytes = coefficient.to_signed_bytes_le();
                            if bytes.len() > 32 {
                                return Err(ExecutionError::DecimalOverflow);
                            }
                            let mut storage = if bytes.last().is_some_and(|byte| byte & 0x80 != 0) {
                                [0xff; 32]
                            } else {
                                [0; 32]
                            };
                            storage[..bytes.len()].copy_from_slice(&bytes);
                            Ok(i256::from_le_bytes(storage))
                        })
                        .transpose()
                })
                .collect::<DecimalResult<Vec<_>>>()?;
            let array =
                Decimal256Array::from(values).with_precision_and_scale(self.precision, 0)?;
            // Validate coefficients at scale zero: Arrow 59's validator casts
            // negative scales to u8 before checking precision.
            array.validate_decimal_precision(self.precision)?;
            Ok(Arc::new(
                array.with_precision_and_scale(self.precision, self.scale)?,
            ))
        } else {
            let values = rows
                .iter()
                .map(|row| {
                    row.try_get::<Option<BigDecimal>, _>(index)?
                        .map(|value| {
                            let value = self.exact_scale(value)?;
                            let (coefficient, _) = value.as_bigint_and_exponent();
                            i128::try_from(coefficient).map_err(|_| ExecutionError::DecimalOverflow)
                        })
                        .transpose()
                })
                .collect::<DecimalResult<Vec<_>>>()?;
            let array =
                Decimal128Array::from(values).with_precision_and_scale(self.precision, 0)?;
            // Validate coefficients at scale zero: Arrow 59's validator casts
            // negative scales to u8 before checking precision.
            array.validate_decimal_precision(self.precision)?;
            Ok(Arc::new(
                array.with_precision_and_scale(self.precision, self.scale)?,
            ))
        }
    }

    fn exact_scale(self, value: BigDecimal) -> DecimalResult<BigDecimal> {
        let scaled = value.with_scale(i64::from(self.scale));
        if scaled != value {
            return Err(ExecutionError::DecimalRounding { scale: self.scale });
        }
        Ok(scaled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modifier(precision: i32, scale: i32) -> i32 {
        ((precision << 16) | (scale & 0x7ff)) + 4
    }

    #[test]
    fn chooses_width_and_rejects_unrepresentable_declarations() -> DecimalResult<()> {
        assert_eq!(
            DecimalType::from_modifier(modifier(38, 2))?.data_type(),
            DataType::Decimal128(38, 2)
        );
        assert_eq!(
            DecimalType::from_modifier(modifier(39, 2))?.data_type(),
            DataType::Decimal256(39, 2)
        );
        assert_eq!(
            DecimalType::from_modifier(modifier(76, 0))?.data_type(),
            DataType::Decimal256(76, 0)
        );
        assert_eq!(
            DecimalType::from_modifier(modifier(2, -3))?.data_type(),
            DataType::Decimal128(2, -3)
        );
        assert!(DecimalType::from_modifier(modifier(77, 0)).is_err());
        assert!(DecimalType::from_modifier(modifier(2, 128)).is_err());
        assert!(DecimalType::from_modifier(-1).is_err());
        Ok(())
    }

    #[test]
    fn rescaling_never_rounds() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let decimal = DecimalType::from_modifier(modifier(10, 2))?;
        assert!(decimal.exact_scale("1.234".parse()?).is_err());
        assert_eq!(
            decimal.exact_scale("1.230000".parse()?)?,
            "1.23".parse::<BigDecimal>()?
        );
        Ok(())
    }
}

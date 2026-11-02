CREATE TABLE IF NOT EXISTS type_coverage (
 id INT PRIMARY KEY,
 tiny_value TINYINT, small_value SMALLINT, medium_value MEDIUMINT, int_value INT, big_value BIGINT,
 unsigned_tiny TINYINT UNSIGNED, unsigned_small SMALLINT UNSIGNED, unsigned_medium MEDIUMINT UNSIGNED,
 unsigned_int INT UNSIGNED, unsigned_big BIGINT UNSIGNED,
 decimal_value DECIMAL(30,10), float_value FLOAT, double_value DOUBLE, bit_value BIT(8), bool_value BOOLEAN,
 date_value DATE, time_value TIME, datetime_value DATETIME(6), timestamp_value TIMESTAMP(6) NULL, year_value YEAR,
 char_value CHAR(3), varchar_value VARCHAR(30), binary_value BINARY(3), varbinary_value VARBINARY(30),
 tinytext_value TINYTEXT, text_value TEXT, mediumtext_value MEDIUMTEXT, longtext_value LONGTEXT,
 tinyblob_value TINYBLOB, blob_value BLOB, mediumblob_value MEDIUMBLOB, longblob_value LONGBLOB,
 enum_value ENUM('happy','sad'), set_value SET('a','b'), json_value JSON,
 geometry_value GEOMETRY, point_value POINT, linestring_value LINESTRING, polygon_value POLYGON,
 multipoint_value MULTIPOINT, multilinestring_value MULTILINESTRING, multipolygon_value MULTIPOLYGON,
 geometrycollection_value GEOMETRYCOLLECTION
);
DELETE FROM type_coverage;
INSERT INTO type_coverage VALUES (
 1,1,2,3,4,5,1,2,3,4,18446744073709551615,123.456,1.25,2.5,b'10101010',true,
 '2024-01-02','12:34:56','2024-01-02 12:34:56.123456','2024-01-02 12:34:56.123456',2024,
 'abc','hello','abc','hello','hello','hello','hello','hello','abc','abc','abc','abc','happy','a,b','{"a":1}',
 ST_GeomFromText('POINT(1 2)'),ST_GeomFromText('POINT(1 2)'),ST_GeomFromText('LINESTRING(0 0,1 1)'),
 ST_GeomFromText('POLYGON((0 0,0 1,1 1,0 0))'),ST_GeomFromText('MULTIPOINT(1 2,3 4)'),
 ST_GeomFromText('MULTILINESTRING((0 0,1 1))'),ST_GeomFromText('MULTIPOLYGON(((0 0,0 1,1 1,0 0)))'),
 ST_GeomFromText('GEOMETRYCOLLECTION(POINT(1 2))')
);
INSERT INTO type_coverage (id) VALUES (2);

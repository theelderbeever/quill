-- Isolated schema: reruns replace only these fixtures.
DROP SCHEMA IF EXISTS type_coverage CASCADE;
CREATE SCHEMA type_coverage;
SET search_path = type_coverage, public;
CREATE TYPE mood AS ENUM ('happy', 'sad');
CREATE TYPE pair AS (label text, value integer);
CREATE DOMAIN positive_integer AS integer CHECK (VALUE > 0);
CREATE TABLE all_types (
 id integer PRIMARY KEY, smallint_value smallint, integer_value integer, bigint_value bigint,
 numeric_value numeric(30,10), real_value real, double_value double precision, money_value money,
 boolean_value boolean, char_value char(3), varchar_value varchar(20), text_value text, bytea_value bytea,
 date_value date, time_value time, timetz_value timetz, timestamp_value timestamp, timestamptz_value timestamptz,
 interval_value interval, uuid_value uuid, json_value json, jsonb_value jsonb, xml_value xml,
 bit_value bit(3), varbit_value varbit(8), inet_value inet, cidr_value cidr, macaddr_value macaddr, macaddr8_value macaddr8,
 point_value point, line_value line, lseg_value lseg, box_value box, path_value path, polygon_value polygon, circle_value circle,
 tsvector_value tsvector, tsquery_value tsquery, pg_lsn_value pg_lsn, xid8_value xid8,
 int4range_value int4range, int8range_value int8range, numrange_value numrange,
 daterange_value daterange, tsrange_value tsrange, tstzrange_value tstzrange,
 int4multirange_value int4multirange, int8multirange_value int8multirange, nummultirange_value nummultirange,
 datemultirange_value datemultirange, tsmultirange_value tsmultirange, tstzmultirange_value tstzmultirange,
 array_value integer[], multidimensional_value text[][], enum_value mood, composite_value pair, domain_value positive_integer
);
-- Populate every declared type via PostgreSQL's own input parser.
DO $$
DECLARE values_sql text;
BEGIN
 SELECT string_agg(quote_literal(v), ', ' ORDER BY n) INTO values_sql FROM unnest(ARRAY[
 '1','12','34','56','123.456','1.25','2.5','12.34','true','abc','hello','hello','\x0102',
 '2024-01-02','12:34:56','12:34:56+00','2024-01-02 12:34:56','2024-01-02 12:34:56+00','1 month 2 days',
 '00000000-0000-4000-8000-000000000001','{"a":1}','{"a":1}','<a>1</a>',
 '101','10101','192.0.2.1','192.0.2.0/24','08:00:2b:01:02:03','08:00:2b:01:02:03:04:05',
 '(1,2)','{1,2,3}','[(1,2),(3,4)]','(1,2),(3,4)','[(1,2),(3,4)]','((1,2),(3,4),(5,6))','<(1,2),3>',
 'hello','hello & world','0/16B6C50','123','[1,5)','[1,5)','[1,5)',
 '[2024-01-01,2024-02-01)','[2024-01-01,2024-02-01)','[2024-01-01,2024-02-01)',
 '{[1,5)}','{[1,5)}','{[1,5)}','{[2024-01-01,2024-02-01)}','{[2024-01-01,2024-02-01)}','{[2024-01-01,2024-02-01)}',
 '{1,2}','{{a,b},{c,d}}','happy','(hello,1)','1'
 ]) WITH ORDINALITY AS x(v,n);
 EXECUTE 'INSERT INTO all_types VALUES (' || values_sql || ')';
END $$;
INSERT INTO all_types (id) VALUES (2);

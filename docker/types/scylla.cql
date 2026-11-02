CREATE KEYSPACE IF NOT EXISTS type_coverage WITH replication = {'class': 'SimpleStrategy', 'replication_factor': 1};
USE type_coverage;
CREATE TYPE IF NOT EXISTS pair (label text, value int);
CREATE TABLE IF NOT EXISTS all_types (
 id int PRIMARY KEY, ascii_value ascii, bigint_value bigint, blob_value blob, boolean_value boolean,
 date_value date, decimal_value decimal, double_value double, duration_value duration, float_value float,
 inet_value inet, int_value int, smallint_value smallint, text_value text, time_value time,
 timestamp_value timestamp, timeuuid_value timeuuid, tinyint_value tinyint, uuid_value uuid, varchar_value varchar,
 varint_value varint, list_value list<int>, set_value set<text>, map_value map<text,int>,
 tuple_value tuple<int,text>, udt_value frozen<pair>, nested_value frozen<list<list<int>>>
);
TRUNCATE all_types;
INSERT INTO all_types (id,ascii_value,bigint_value,blob_value,boolean_value,date_value,decimal_value,double_value,duration_value,float_value,inet_value,int_value,smallint_value,text_value,time_value,timestamp_value,timeuuid_value,tinyint_value,uuid_value,varchar_value,varint_value,list_value,set_value,map_value,tuple_value,udt_value,nested_value)
VALUES (1,'hello',123,0x0102,true,'2024-01-02',123.456,2.5,1mo2d,1.25,'192.0.2.1',42,12,'hello','12:34:56','2024-01-02T12:34:56Z',50554d6e-29bb-11e5-b345-feff819cdc9f,1,00000000-0000-4000-8000-000000000001,'hello',123456789012345678901234567890,[1,2],{'a','b'},{'a':1},(1,'hello'),{label:'hello',value:1},[[1,2],[3,4]]);
INSERT INTO all_types (id) VALUES (2);
-- Counters cannot share a table with non-counter value columns.
CREATE TABLE IF NOT EXISTS counters (id int PRIMARY KEY, counter_value counter);
TRUNCATE counters;
UPDATE counters SET counter_value = counter_value + 1 WHERE id = 1;

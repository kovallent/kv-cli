select a from t qualify row_number() over (partition by a order by b desc) = 1
select a, b, from t
select a, b, from t where x = 1
select a, b, from t qualify row_number() over (partition by a order by b desc) = 1
select * except(a) from t
select first_value(x ignore nulls) over (partition by a) from t
select first_value(x) over (w) from t window w as (partition by a order by b)
select first_value(x ignore nulls) over (w) from t window w as (partition by a order by b)
select * from read_files('/Volumes/c/s/v', format => 'json')
select * from stream read_files('/Volumes/c/s/v')
select * from stream read_files('/Volumes/c/s/v', format => 'json')
select from_json(payload, 'struct<a:int>') as p from t
select explode(items) as item from t
select x::timestamp, named_struct('a', 1), _metadata.file_path from t

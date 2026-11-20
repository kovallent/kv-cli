-- Clean: information_schema is the same in every environment.
select table_name, column_name
from analytics.information_schema.columns
where table_schema = 'RAW'

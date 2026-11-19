-- Clean: a source(), a CTE and an alias. Nothing here names a database.
with source as (
    select * from {{ source('shop', 'raw_orders') }}
)

select
    o.order_id,
    o.customer_id,
    o.amount
from source as o

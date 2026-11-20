-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises Snowflake syntax: VARIANT paths (raw:field::type), LATERAL
-- FLATTEN with a named argument, IFF, TRY_TO_NUMBER and QUALIFY.
{{ config(materialized='view') }}

with source as (
    select * from {{ source('shop', 'raw_orders') }}
),

lines as (
    select
        source.raw:order_id::string as order_id,
        source.raw:customer.id::string as customer_id,
        line.value:sku::string as sku,
        try_to_number(line.value:qty::string) as quantity,
        iff(line.value:gift::boolean, 'gift', 'standard') as line_kind,
        source.loaded_at
    from source,
        lateral flatten(input => source.raw:lines) as line
)

select *
from lines
qualify row_number() over (partition by order_id, sku order by loaded_at desc) = 1

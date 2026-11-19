-- Pinned: two warehouse tables named outright, around dbt. Each is its own
-- finding with its own fingerprint. The ref() is fine.
select
    o.order_id,
    o.amount,
    c.region
from ANALYTICS_PROD.RAW.ORDERS as o
join ANALYTICS_PROD.RAW.CUSTOMERS as c on c.customer_id = o.customer_id
join {{ ref('stg_orders') }} as s on s.order_id = o.order_id

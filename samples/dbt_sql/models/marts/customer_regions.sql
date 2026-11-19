-- A literal schema: reported only with `sql.two_part_names: true`, since a
-- schema pins the model only where schemas vary by target.
select
    c.region,
    count(*) as customers
from RAW.CUSTOMERS as c
join {{ ref('orders') }} as o on o.region = c.region
group by c.region

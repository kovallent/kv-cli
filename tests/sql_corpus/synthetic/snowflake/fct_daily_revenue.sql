-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises an incremental model: is_incremental(), {{ this }}, DATEADD,
-- DATE_TRUNC, and a Jinja for loop over a list set in the model.
{{ config(materialized='incremental', unique_key='revenue_date') }}

{% set currencies = ['usd', 'eur', 'gbp'] %}

with orders as (
    select * from {{ ref('stg_orders') }}
    {% if is_incremental() %}
    where ordered_at > (select dateadd(day, -3, max(revenue_date)) from {{ this }})
    {% endif %}
)

select
    date_trunc('day', ordered_at) as revenue_date,
    {% for currency in currencies %}
    sum(case when currency = '{{ currency }}' then amount else 0 end) as revenue_{{ currency }},
    {% endfor %}
    count(*) as order_count
from orders
group by 1

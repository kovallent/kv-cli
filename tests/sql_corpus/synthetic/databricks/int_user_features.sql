-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises Databricks syntax: from_json, explode, :: casts, date_sub,
-- datediff, named_struct, collect_list and QUALIFY.
with events as (
    select
        user_id,
        from_json(payload, 'struct<items:array<string>, amount:double>') as parsed,
        event_ts::timestamp as event_ts
    from {{ ref('stg_streaming_events') }}
    where event_ts >= date_sub(current_date(), 30)
),

items as (
    select
        user_id,
        event_ts,
        explode(parsed.items) as item,
        parsed.amount as amount
    from events
)

select
    user_id,
    named_struct('first_seen', min(event_ts), 'last_seen', max(event_ts)) as activity,
    datediff(max(event_ts), min(event_ts)) as active_days,
    collect_list(item) as items,
    sum(amount) as total_amount
from items
group by user_id
qualify row_number() over (partition by user_id order by max(event_ts) desc) = 1

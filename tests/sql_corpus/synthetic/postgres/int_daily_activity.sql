-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises PostgreSQL syntax: DISTINCT ON, :: casts, jsonb ->> access,
-- aggregate FILTER (WHERE ...), INTERVAL literals and generate_series.
with events as (
    select
        (payload ->> 'user_id')::bigint as user_id,
        (payload ->> 'occurred_at')::timestamptz as occurred_at,
        payload ->> 'event' as event_name
    from {{ source('app', 'raw_events') }}
),

days as (
    select generate_series(
        date_trunc('day', now()) - interval '30 days',
        date_trunc('day', now()),
        interval '1 day'
    )::date as day
),

latest as (
    select distinct on (user_id)
        user_id,
        occurred_at as last_seen_at
    from events
    order by user_id, occurred_at desc
)

select
    days.day,
    count(*) filter (where events.event_name = 'login') as logins,
    count(distinct events.user_id) as active_users,
    max(latest.last_seen_at) as last_seen_at
from days
left join events on events.occurred_at::date = days.day
left join latest on latest.user_id = events.user_id
group by days.day

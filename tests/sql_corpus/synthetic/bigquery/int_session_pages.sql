-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises BigQuery syntax: SELECT * EXCEPT, UNNEST WITH OFFSET over a
-- correlated path, STRUCT, SAFE_CAST, ARRAY_AGG of a STRUCT, an array literal
-- and QUALIFY.
with sessions as (
    select * except (_loaded_at)
    from {{ source('analytics', 'sessions') }}
    where session_date >= date_sub(current_date(), interval 7 day)
),

pages as (
    select
        sessions.session_id,
        page.url,
        safe_cast(page.duration_ms as int64) as duration_ms,
        page_index
    from sessions,
        unnest(sessions.pages) as page with offset as page_index
)

select
    session_id,
    array_agg(struct(url, duration_ms) order by page_index) as page_path,
    [1, 2, 3] as sample_buckets
from pages
group by session_id
qualify count(*) over (partition by session_id) > 0

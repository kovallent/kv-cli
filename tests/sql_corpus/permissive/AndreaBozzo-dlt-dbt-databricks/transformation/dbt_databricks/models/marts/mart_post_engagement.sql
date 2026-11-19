-- Mart: one row per post with engagement metrics.
-- Materialized as an INCREMENTAL model using Databricks MERGE: on a normal run dbt only processes
-- posts touched by the latest dlt load (filtered via _dlt_load_id) and upserts them on post_id.
-- Full-refresh with: dbt build --full-refresh -s mart_post_engagement
{{
    config(
        materialized='incremental',
        incremental_strategy='merge',
        unique_key='post_id',
        file_format='delta',
    )
}}

with posts as (
    select * from {{ ref('stg_rest_posts') }}
),

comment_counts as (
    select * from {{ ref('int_post_comment_counts') }}
)

select
    p.post_id,
    p.user_id,
    p.title,
    coalesce(c.comment_count, 0)        as comment_count,
    coalesce(c.distinct_commenters, 0)  as distinct_commenters,
    p._dlt_load_id                      as _dlt_load_id
from posts p
left join comment_counts c on p.post_id = c.post_id

{% if is_incremental() %}
-- Only reprocess rows from dlt loads newer than what we've already materialized.
where p._dlt_load_id > (select coalesce(max(_dlt_load_id), '') from {{ this }})
{% endif %}

-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises PostgreSQL syntax: a LATERAL subquery, STRING_AGG with ORDER BY,
-- UNNEST with a column alias list, numeric casts with precision, and an
-- if/else on a dbt var.
{{ config(materialized='table') }}

with places as (
    select * from {{ ref('stg_places') }}
)

select
    places.place_id,
    places.name,
    {% if var('include_geometry', false) %}
    places.geom,
    {% else %}
    null as geom,
    {% endif %}
    round(places.rating::numeric(4, 2), 1) as rating,
    tags.tag_list,
    tags.tag_count
from places
cross join lateral (
    select
        string_agg(t.tag, ', ' order by t.tag) as tag_list,
        count(*) as tag_count
    from unnest(places.tags) as t(tag)
) as tags

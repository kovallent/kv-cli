-- SYNTHETIC: written for this spike, not taken from any project.
-- Exercises Databricks syntax: a streaming table that reads files with
-- STREAM read_files(...) and named arguments (=>), from a Volumes path that
-- varies by target.
{{ config(materialized='streaming_table') }}

select
    *,
    _metadata.file_path as source_file,
    current_timestamp() as ingested_at
from stream read_files(
    '/Volumes/{{ target.catalog }}/landing/events',
    format => 'json',
    schemaHints => 'event_id string, event_ts timestamp'
)

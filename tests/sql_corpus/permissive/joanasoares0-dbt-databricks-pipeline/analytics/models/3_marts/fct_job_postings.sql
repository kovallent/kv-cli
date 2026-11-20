{{
    config(
        materialized='incremental',
        incremental_strategy='merge',
        unique_key='job_id'
    )
}}

WITH job_postings AS (
    SELECT * FROM {{ ref('stg_job_postings') }}
)

SELECT
    job_id,
    job_title,
    search_term,
    company_name,
    job_location,
    source_platform,
    job_schedule_type,
    job_work_from_home,
    search_date,
    salary_min,
    salary_max,
    salary_avg,
    salary_period,
    salary_currency,
    has_health_insurance,
    has_dental_insurance,
    has_paid_time_off,
    has_no_degree_mentioned
FROM job_postings

{% if is_incremental() %}
-- On incremental runs, only process days newer than what's already loaded.
-- `>` works for this dataset. If rows could arrive late on the same day,
-- `>=` would be safer: the merge on job_id makes reprocessing a day harmless.
WHERE search_date > (SELECT MAX(search_date) FROM {{ this }})
{% endif %}

-- One row per job: keep the most recent scrape.
QUALIFY ROW_NUMBER() OVER (
    PARTITION BY job_id
    ORDER BY searched_at DESC
) = 1
{#-
    One row per job posting per scrape day, not one row per job.
    The same job can appear several times if it was scraped more than once.
    We keep those repeats on purpose: the snapshot uses them to track changes,
    and fct_job_postings reduces them to one row per job.
-#}

{% set extension_keywords = [
    'Health insurance',
    'Dental insurance',
    'Paid time off',
    'No degree mentioned'
] %}

--filter source (always filter as upstream as possible to reduce the amount of data processed)
WITH source AS (

    SELECT *
    FROM {{ source('jobs', 'raw_job_postings') }}

    -- Drop failed scrapes. The scraper sets error = TRUE on its 151,147 failed
    -- rows and leaves it NULL on good rows. We use IS NOT TRUE because
    -- `error = FALSE` would also drop the NULL (good) rows.
    -- The failed rows are still kept in the raw table.
    WHERE error IS NOT TRUE

    -- Drop the 6,255 postings with no job_id. They are real postings, but
    -- without an id we can't remove duplicates or join them to skills.
      AND job_id IS NOT NULL
),

-- Parse the extensions JSON once here, so the loop below can reuse the array.
parsed AS (

    SELECT
        *,
        FROM_JSON(job_extensions_raw, 'array<string>') AS job_extensions
    FROM source

),

-- Clean data and parse salaries. The salary parsing is done in a macro.
cleaned AS (

    SELECT
        -- ids + scrape metadata
        job_id,
        search_term,
        search_date,
        search_time AS searched_at,
        search_location,

        -- posting attributes
        job_title,
        TRIM(company_name) AS company_name,
        job_location,
        REGEXP_REPLACE(job_via, '^via ', '') AS source_platform,  -- 'via LinkedIn' -> 'LinkedIn' (~6.3k rows have the 'via ' prefix)
        job_schedule_type,
        job_work_from_home,

        -- Relative text like '3 days ago', kept as-is. We use search_date for
        -- dates instead: ~95% of postings are scraped within a day of being
        -- posted, so the two are almost always the same.
        job_posted_at,

        -- Salary text turned into numbers. The original text is kept too,
        -- so the snapshot can spot when a posting changes its salary.
        job_salary,
        {{ parse_salary('min') }} AS salary_min,
        {{ parse_salary('max') }} AS salary_max,

        -- Main salary measure: the middle of the range (a single value is used as-is).
        -- It mixes currencies and periods (hourly, yearly...), so filter on
        -- salary_currency and salary_period before averaging it.
        ({{ parse_salary('min') }} + {{ parse_salary('max') }}) / 2 AS salary_avg,

        NULLIF(REGEXP_EXTRACT(job_salary, 'an? (year|hour|month|day|week)$', 1), '') AS salary_period,
        
        -- Currency comes from the text before the number ('PKR', 'CA$', '₱', ...).
        -- If there's a salary but no currency marker, we set 'USD', so you can
        -- simply filter on salary_currency = 'USD'. No salary means NULL.
        CASE
            WHEN job_salary IS NOT NULL
            THEN COALESCE(NULLIF(TRIM(REGEXP_EXTRACT(job_salary, '^([^0-9]+)', 1)), ''), 'USD')
        END AS salary_currency,


        -- One TRUE/FALSE column per keyword in extension_keywords (top of file),
        -- e.g. 'Paid time off' becomes has_paid_time_off.
        {% for keyword in extension_keywords %}
        ARRAY_CONTAINS(
            job_extensions,
            '{{ keyword }}'
        ) AS has_{{ slugify(keyword) }}
        {{- "," if not loop.last }}
        {% endfor %}

    FROM parsed
)

SELECT * FROM cleaned
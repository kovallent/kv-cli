-- SYNTHETIC: written for kv-cli's SQL corpus, not taken from any project.
-- Exercises a snapshot block, whose body is a select once neutralised.
{% snapshot snp_customers %}

{{
    config(
        target_schema='snapshots',
        unique_key='customer_id',
        strategy='timestamp',
        updated_at='updated_at',
    )
}}

select * from {{ source('crm', 'customers') }}

{% endsnapshot %}

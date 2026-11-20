-- Staging for the synthetic insurance claims (samples.healthverity).
-- Raw data is all strings at claim-line-detail grain. We cast types, normalize codes, and add a
-- surrogate key (the raw table has no natural unique key — rows repeat per diagnosis/procedure).
with source as (
    select * from {{ source('healthverity', 'claims_sample_synthetic') }}
)

select
    {{ dbt_utils.generate_surrogate_key([
        'claim_id', 'service_line_number', 'diagnosis_code',
        'procedure_code', 'revenue_code', 'date_service'
    ]) }}                                            as claim_line_sk,
    claim_id,
    hvid,
    cast(service_line_number as int)                 as service_line_number,
    case claim_type
        when 'P' then 'professional'
        when 'I' then 'institutional'
        else 'unknown'
    end                                              as claim_type,
    try_cast(date_service as date)                   as service_date,
    upper(nullif(payer_type, ''))                    as payer_type,
    nullif(diagnosis_code, '')                       as diagnosis_code,
    nullif(procedure_code, '')                       as procedure_code,
    patient_gender,
    try_cast(patient_year_of_birth as int)           as patient_year_of_birth,
    patient_state,
    patient_zip3,
    -- Money: charged by provider vs allowed by payer. allowed = 0 → denied / not covered.
    -- ~57% of line_charge is NULL and ~5.8k rows are negative (claim reversals/adjustments).
    try_cast(line_charge as double)                  as line_charge,
    try_cast(line_allowed as double)                 as line_allowed,
    coalesce(try_cast(line_charge as double) < 0, false) as is_reversal,
    prov_billing_npi,
    prov_rendering_npi
from source

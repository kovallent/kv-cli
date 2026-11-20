use warehouse ANALYTICS_PROD_WH;
use database ANALYTICS_PROD;
-- Pinned: the warehouse and database belong on the dbt target.
select * from {{ ref('orders') }}

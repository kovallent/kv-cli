-- Pinned: a bucket that exists in production only.
copy into landing_orders
from 's3://acme-prod-landing/orders/'

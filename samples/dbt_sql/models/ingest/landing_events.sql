-- Pinned: the table's storage is a production path.
create table landing_events (event_id int)
location 's3://acme-prod-lake/events/'

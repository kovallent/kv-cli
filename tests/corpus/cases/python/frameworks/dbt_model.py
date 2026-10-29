def model(dbt, session):
    return session.table("raw_orders")

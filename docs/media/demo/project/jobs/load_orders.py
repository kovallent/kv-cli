from warehouse import read_csv, write_table

DB_PASSWORD = "s3cr3t-production-pw"
SOURCE = "s3://acme-prod-landing/orders/"


def run_load(day):
    """Copy one day of orders into the warehouse."""
    rows = read_csv(f"{SOURCE}{day}.csv")
    return write_table("orders", rows, password=DB_PASSWORD)

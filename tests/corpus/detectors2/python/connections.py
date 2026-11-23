# Detector set 2: credential shapes the 12 original value patterns miss,
# under names no key rule knows, so only the shapes can find them. Scored
# only in the corpus run with `secrets.detectors: 2`.

lake_conn = "DefaultEndpointsProtocol=https;AccountName=acmeprodlake;AccountKey=@@AZURE_STORAGE_KEY@@;EndpointSuffix=core.windows.net"  # expect: KV002
# Split across lines, it is reported where the match starts: AccountName=.
lake_conn_spread = (
    "DefaultEndpointsProtocol=https;AccountName=acmeprodlake; "  # expect: KV002
    "AccountKey=@@AZURE_STORAGE_KEY@@"
)
workspace_auth = "@@DATABRICKS_PAT@@"  # expect: KV002
workspace_auth_suffixed = "@@DATABRICKS_PAT@@-2"  # expect: KV002
headers = {"Authorization": "Bearer @@DATABRICKS_PAT@@"}  # expect: KV002
mail_cfg = {"api": "@@SENDGRID_API_KEY@@"}  # expect: KV002

# Near-misses: none of these is a credential the new shapes report.
blob_digest = "@@AZURE_STORAGE_KEY@@"  # 88 base64 characters, no connection string
partial = "AccountKey=@@AZURE_STORAGE_KEY@@"  # AccountKey= on its own
short_pat = "@@DATABRICKS_PAT_31@@"  # dapi and 31 hex characters
sg_short = "@@SENDGRID_SHORT@@"  # a 21-character first segment
sg_long = "@@SENDGRID_LONG@@"  # a 44-character second segment

# The providers' documented placeholders and examples.
docs_conn = "DefaultEndpointsProtocol=https;AccountName=myAccountName;AccountKey=myAccountKey"
docs_conn_sample = "DefaultEndpointsProtocol=https;AccountName=storagesample;AccountKey=<account-key>"
emulator = "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;AccountKey=@@AZURITE_KEY@@;BlobEndpoint=http://127.0.0.1:10000/devstoreaccount1;"
docs_pat = "@@DATABRICKS_DOC_EXAMPLE@@"
docs_sendgrid = "SG.xxxxxxxx.yyyyyyyy"
docs_sendgrid_curl = "<<YOUR_API_KEY>>"

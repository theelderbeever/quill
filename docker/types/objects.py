# /// script
# requires-python = ">=3.11"
# dependencies = ["boto3>=1.35,<2", "pyarrow>=18,<24"]
# ///
"""Generate matching datasets and upload them to the development RustFS bucket."""
import csv
import io
import json
import os
from pathlib import Path

import boto3
from botocore.exceptions import ClientError
import pyarrow as pa
import pyarrow.parquet as pq

rows = [
    {"id": 1, "name": "Ada", "city": "London", "logins": 12},
    {"id": 2, "name": "Grace", "city": "New York", "logins": 7},
    {"id": 3, "name": "Linus", "city": "Helsinki", "logins": 0},
]
text = io.StringIO()
writer = csv.DictWriter(text, fieldnames=list(rows[0]))
writer.writeheader()
writer.writerows(rows)
parquet = io.BytesIO()
pq.write_table(pa.Table.from_pylist(rows), parquet)
datasets = {
    "people_csv/people.csv": text.getvalue().encode(),
    "people_json/people.json": "".join(json.dumps(row) + "\n" for row in rows).encode(),
    "people_parquet/people.parquet": parquet.getvalue(),
}
endpoint = os.environ.get("RUSTFS_ENDPOINT", "https://localhost:9000")
client = boto3.client(
    "s3", endpoint_url=endpoint, region_name="us-east-1",
    aws_access_key_id="quill", aws_secret_access_key="password",
    verify=str(Path("docker/tls/certs/ca.pem").resolve()),
)
try:
    client.create_bucket(Bucket="quill")
except ClientError as error:
    if error.response["Error"]["Code"] != "BucketAlreadyOwnedByYou":
        raise
for key, data in datasets.items():
    client.put_object(Bucket="quill", Key=key, Body=data)
    assert client.get_object(Bucket="quill", Key=key)["Body"].read() == data
    print(f"Seeded s3://quill/{key} ({len(data)} bytes)")

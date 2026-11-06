"""Generate the fictional orders used in the README screenshots (requires pyarrow)."""

from datetime import date, timedelta
from decimal import Decimal
from pathlib import Path
from random import Random

import pyarrow as pa
import pyarrow.parquet as pq


rng = Random(42)
products = [
    ("Studio Monitor", 24900),
    ("Desk Lamp", 7900),
    ("Mechanical Keys", 12900),
    ("USB-C Hub", 6900),
    ("Laptop Stand", 8900),
    ("Wireless Mouse", 5900),
    ("Desk Mat", 3900),
    ("Webcam", 14900),
]
regions = ["Europe", "North America", "Asia Pacific", "Latin America", "Middle East"]
rows = []
for i in range(5000):
    product, cents = rng.choice(products)
    region = rng.choices(regions, weights=[38, 30, 18, 9, 5])[0]
    units = rng.randint(1, 8)
    rows.append(
        {
            "order_id": 100001 + i,
            "product": product,
            "region": region,
            "units": units,
            "total_usd": Decimal(cents * units) / 100,
            "shipped": rng.random() < 0.86,
            "ordered_on": date(2026, 1, 1) + timedelta(days=rng.randrange(273)),
            "channel": rng.choices(["Online", "Retail", "Partner"], weights=[65, 25, 10])[0],
        }
    )

schema = pa.schema(
    [
        ("order_id", pa.int64()),
        ("product", pa.string()),
        ("region", pa.string()),
        ("units", pa.int32()),
        ("total_usd", pa.decimal128(10, 2)),
        ("shipped", pa.bool_()),
        ("ordered_on", pa.date32()),
        ("channel", pa.string()),
    ]
)
table = pa.Table.from_pylist(rows, schema=schema)
pq.write_table(table, Path(__file__).with_name("sales.parquet"), compression="snappy")

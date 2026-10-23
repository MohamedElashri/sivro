"""Regenerate interoperability fixtures with `uv run --with pyarrow generate.py`."""
from decimal import Decimal
from pathlib import Path

import pyarrow as pa
import pyarrow.feather as feather
import pyarrow.ipc as ipc

root = Path(__file__).parent
schema = pa.schema(
    [
        pa.field("id", pa.int64(), metadata={"unit": "count"}),
        pa.field("unsigned", pa.uint64()),
        pa.field("amount", pa.decimal128(38, 8)),
        pa.field("label", pa.dictionary(pa.int8(), pa.string())),
        pa.field("nested", pa.list_(pa.int64())),
        pa.field("utc_ns", pa.timestamp("ns", tz="UTC")),
        pa.field("bytes", pa.binary()),
    ],
    metadata={"producer": "PyArrow", "fixture": "synthetic interoperability"},
)
batch = pa.RecordBatch.from_arrays(
    [
        pa.array([9007199254740993, None], type=pa.int64()),
        pa.array([2**64 - 1, 0], type=pa.uint64()),
        pa.array([Decimal("123456789012345678901234567890.12345678"), None], type=pa.decimal128(38, 8)),
        pa.array(["Alice", None], type=pa.dictionary(pa.int8(), pa.string())),
        pa.array([[9007199254740993, None], []], type=pa.list_(pa.int64())),
        pa.array([1700000000123456789, None], type=pa.timestamp("ns", tz="UTC")),
        pa.array([b"\x00\xff", b""], type=pa.binary()),
    ],
    schema=schema,
)
feather.write_feather(pa.Table.from_batches([batch]), root / "pyarrow.feather", compression="lz4")
with ipc.new_file(root / "pyarrow.arrow", schema, options=ipc.IpcWriteOptions(compression="zstd")) as writer:
    writer.write_batch(batch)
with ipc.new_stream(root / "pyarrow.arrows", schema, options=ipc.IpcWriteOptions(compression="zstd")) as writer:
    writer.write_batch(batch)
(root / "README.md").write_text(
    f"Synthetic fixtures generated with PyArrow {pa.__version__}.\n\n"
    "Regenerate with `uv run --with pyarrow generate.py`.\n"
    "Files cover IPC files/streams, Feather V2, LZ4/ZSTD, dictionaries, nulls,\n"
    "exact integers/decimals, nested values, zoned nanoseconds and binary.\n"
)

"""Synthetic interoperability fixtures: uv run --with h5py generate.py."""
from pathlib import Path
import h5py
import numpy as np

root = Path(__file__).parent
for filename, version, userblock in [("classic.h5", "earliest", 0), ("modern.hdf5", "v114", 0), ("userblock.hdf", "v114", 512)]:
    with h5py.File(root / filename, "w", libver=version, userblock_size=userblock) as f:
        f.attrs["producer"] = "h5py"
        f.attrs["wide"] = np.uint64(2**64 - 1)
        f.attrs["precise"] = np.array([1.25, np.nan])
        f.create_dataset("scalar", data=np.array(42, dtype=">i8"))
        f.create_dataset("empty", shape=(0, 2), dtype="i8")
        f.create_dataset("null", dtype="i4")
        arrays = f.create_group("arrays")
        arrays.attrs["units"] = "counts"
        cube = np.arange(24, dtype=np.int64).reshape(2, 3, 4) + 9007199254740993
        arrays.create_dataset("cube", data=cube, chunks=(1, 2, 2), compression="gzip", shuffle=True, fletcher32=True)
        arrays.create_dataset("lzf", data=np.arange(12, dtype=">i4").reshape(3, 4), compression="lzf")
        arrays.create_dataset("half", data=np.array([0.0, -0.0, 1.5, np.inf, np.nan], dtype="f2"))
        rows = f.create_group("rows")
        rows.create_dataset("id", data=np.array([9007199254740993, 7], dtype=">i8"))
        rows.create_dataset("name", data=["Alice", "Émile ✓"], dtype=h5py.string_dtype("utf-8"))
        rows.create_dataset("flag", data=np.array([True, False]))
        rows.create_dataset("blob", data=np.array([b"\x00\xff", b"\x01\x02"], dtype="V2"))
        colors = h5py.enum_dtype({"RED": 1, "BLUE": 2}, basetype="u1")
        compound = np.dtype([("id", ">u8"), ("label", h5py.string_dtype("utf-8")), ("samples", "<i8", (2,)), ("color", colors)])
        records = np.empty(2, dtype=compound)
        records["id"] = [2**64 - 1, 7]
        records["label"] = ["Alice", ""]
        records["samples"] = [[9007199254740993, 2], [3, 4]]
        records["color"] = [1, 2]
        f.create_dataset("records", data=records)
        variable = f.create_dataset("variable", (2,), dtype=h5py.vlen_dtype(np.dtype("i4")))
        variable[0] = [1, 2, 3]
        variable[1] = []
        f.create_dataset("references", data=np.array([f["scalar"].ref], dtype=h5py.ref_dtype))
        # Path traversal must remain bounded even in the presence of hard cycles.
        loops = f.create_group("links")
        loops["self"] = loops
        loops["soft"] = h5py.SoftLink("/scalar")
        loops["external"] = h5py.ExternalLink("not-opened.h5", "/secret")
        f.create_dataset("external_storage", shape=(4,), dtype="i4", external=[("not-opened.bin", 0, h5py.h5f.UNLIMITED)])
        if version != "earliest":
            layout = h5py.VirtualLayout(shape=(3,), dtype="i4")
            layout[:] = h5py.VirtualSource("not-opened.h5", "/secret", shape=(3,))
            f.create_virtual_dataset("virtual", layout)
    if userblock:
        with (root / filename).open("r+b") as out:
            out.write(b"Synthetic user block: not HDF4 or a MATLAB reader.\n")
with h5py.File(root / "layout-v5.h5", "w", libver="latest") as f:
    f.create_dataset("new_chunks", data=np.arange(16).reshape(4,4), chunks=(2,2), compression="gzip")
    f.create_dataset("readable", data=np.array([7, 8], dtype="i8"))
(root / "README.md").write_text(
    f"Synthetic fixtures generated with h5py {h5py.__version__}, HDF5 {h5py.version.hdf5_version}, NumPy {np.__version__}.\n\n"
    "Regenerate with `uv run --with h5py generate.py`.\n"
    "Covers classic/latest superblocks, user blocks, attributes, scalar/null/empty\n"
    "datasets, multidimensional chunks, gzip/shuffle/Fletcher32/LZF, endian\n"
    "integers, enums/booleans, compound records, variable-length strings/arrays,\n"
    "opaque/reference bytes, hard cycles and external/virtual storage refusal.\n"
)

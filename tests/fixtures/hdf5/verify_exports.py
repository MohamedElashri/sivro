"""Read Sivro's synthetic interoperability export with the reference library."""
import sys
import h5py

with h5py.File(sys.argv[1], "r") as f:
    cols = list(f["table"].values())
    assert [d.attrs["sivro.name"].decode() for d in cols] == ["id", "u", "d", "b", "s", "bytes"]
    assert int(cols[0][0]) == 9007199254740993
    assert int(cols[1][0]) == 18446744073709551615
    assert cols[2].asstr()[0] == "12345678901234567890.123456789"
    assert list(cols[3][...]) == [1, 0]
    assert cols[4].attrs["sivro.encoding"].decode() == "utf8_hex"
    assert bytes.fromhex(cols[4].asstr()[0]) == b"\0abc\0"
    assert cols[5].asstr()[0] == "00FF"
    assert list(f[cols[0].attrs["sivro.validity"].decode()][...]) == [1, 0]
    assert list(f[cols[3].attrs["sivro.validity"].decode()][...]) == [1, 1]
print("h5py verified exact integers, decimals, booleans, NUL text, binary and null masks")

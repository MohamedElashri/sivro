Vendored from crates.io `hdf5-pure` 0.47.0 (MIT OR Apache-2.0).

- Accept layout v5; array record widths already come from index headers.
- Read v2 B-tree chunk records using their stored width (v4 and v5).
- Bound chunk-tree metadata and validate internal/leaf checksums.

The complete upstream source is retained; [sivro.patch](sivro.patch) isolates
the changes in `data_layout.rs`, `chunked_read.rs` and `btree_v2.rs`.
The manifest omits unpublished development targets/dependencies. Interoperability
tests live in `src-tauri/src/tests/hdf5.rs`; fixtures use h5py/HDF5 2.0.

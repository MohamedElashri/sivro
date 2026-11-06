# Screenshot demo

[sales.parquet](sales.parquet) contains 5,000 entirely fictional orders across
eight products, five regions and three sales channels. It includes integer,
text, exact decimal, boolean and date columns. No customer or personal data is
included. The dataset and generator are covered by the repository's MIT license.

Open the file in Sivro to explore it. To recreate the README view:

1. Maximize the window to approximately 1440 × 785 logical pixels.
2. Hide `order_id`, `ordered_on` and `channel` using the eye icons in the schema.
3. Click `region` in the schema to open its column profile.
4. Keep page 1, 100 rows per page, with no search, filters or sorting.
5. Capture the app window in **Light · Latte** and **Dark · Mocha**, with the
   pointer outside the window and menus closed.

The README uses a `<picture>` element to select the screenshot matching the
reader's color scheme, with the light screenshot as a fallback. The PNGs in
[`../images/`](../images/) are native app captures at 2880 × 1570 pixels (2×),
taken with computer use on macOS using Sivro 0.1.1. They show the schema, grid
and column profile shared with 0.1.2. Recapture them when that interface changes.

## Regenerate the data

The checked-in Parquet file is ready to open; Python is only needed to regenerate
it. With `pyarrow` installed in your Python environment, run from the repository
root:

```sh
python3 docs/demo/generate.py
```

The generator uses a fixed random seed and explicit Arrow types. `total_usd`
uses `DECIMAL(10,2)` so monetary values remain exact. Running the script replaces
only `docs/demo/sales.parquet`.

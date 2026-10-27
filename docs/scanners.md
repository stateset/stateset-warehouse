# Scanner setup

Any scanner that types each label and then presses Enter works with the dashboard scanner panel and the warehouse terminal. GS1 supplier labels need one more setting so the dashboard recognises them. No scanner hardware has been accepted yet: test every model with the steps below before using it on the floor.

## What the WMS expects

- **One label per scan, then Enter.** The terminal treats CRLF as one submission.
- **Exactly the printed characters.** Bin, SKU, lot and serial labels are matched literally and case-sensitively. An added space, prefix or suffix stops a label from matching. Nothing is saved when a scan doesn't match. The one exception: the dashboard scanner panel also accepts a bin, SKU or lot label after a leading symbology identifier such as `]C0`. Forms, serial fields and the terminal record scans as typed, so keep code IDs off anyway.
- **GS1 labels in one of two forms.** At the dashboard's product step, a GS1 label must arrive either in the bracketed form `(01)09506000134352(17)261231(10)LOT-7`, or as raw data that starts with a `]C1`, `]d2` or `]Q3` symbology identifier, with the group separator (ASCII 29) after each variable-length field. Raw GS1 data with neither looks like a plain SKU. A label whose only variable-length field (the lot) comes last carries no separator at all, so without a prefix it fails as a SKU mismatch.
- **A reviewed GTIN.** A GS1 label identifies a product only when an administrator has added its GTIN to that product. See [scanning GS1 barcodes](api.md#scanning-gs1-barcodes).

The terminal doesn't parse GS1 labels; scan the product's SKU barcode there.

## Zebra (DataWedge)

Create a DataWedge profile for the browser that opens the dashboard and set the following. Setting names are Zebra's, from the DataWedge guides linked below.

| Setting | Where in DataWedge | Value | Why |
|---|---|---|---|
| Code ID Type | Barcode input, scan parameters | None (the default) | A code ID is added to every barcode. Only the scanner panel removes it, so serial and form scans would stop matching. |
| Send ENTER key | Basic data formatting | On | Submits each scan. |
| Prefix to data, Suffix to data | Basic data formatting | Empty | Labels must arrive unchanged. |
| Send Control Characters as Events | Keystroke output, key event options | Off (the default) | Sends control characters, including the GS1 separator, as text. |
| Include parentheses around AI field | Keystroke output, data formatting (GS1 parsing) | On, with Send tokens disabled | GS1 labels then arrive in the bracketed form, which the WMS recognises without a prefix. |
| Decoders | Barcode input | Only the symbologies on site, such as Code 128 (including GS1-128), Data Matrix and QR | Fewer decoders mean fewer misreads. |

The bracketed form can't represent a lot or serial that itself contains `(`: such a label can be rejected or misread. For those products, scan the SKU and lot barcodes separately instead of the GS1 label.

Zebra references: [Keystroke output](https://techdocs.zebra.com/datawedge/latest/guide/output/keystroke/), [Basic data formatting](https://techdocs.zebra.com/datawedge/latest/guide/process/bdf/), [Barcode input](https://techdocs.zebra.com/datawedge/latest/guide/input/barcode/).

## Other scanners

Honeywell and other keyboard-wedge scanners need the same outcome, though their setting names differ by model: an Enter suffix, no prefix or code ID, and GS1 output in the bracketed form. If a scanner can't produce the bracketed form, it must send the symbology identifier and the group separator for GS1 labels only. Use the vendor's configuration tool to set this.

## Test each scanner model

Do this once per model and configuration, on the deployed dashboard, before the pilot.

1. Print a bin label, a product label from **Print labels**, and supplier-style GS1 labels: a GS1-128 with `(01)(17)(10)`, a GS1-128 with `(01)(10)(21)` (a separator follows the lot) and a GS1 Data Matrix.
2. Add the test GTIN to a product, then start a pick or receipt in the scanner panel.
3. Scan the bin and SKU labels: each must match on the first scan, with its case unchanged.
4. Scan each GS1 label at the product step: the panel must name the expected product and lot.
5. Scan a label for the wrong product: the panel must refuse it and save nothing.
6. Record the scanner model, firmware, DataWedge version, the exported profile and each result. These records feed [facility acceptance](facility-acceptance.md).

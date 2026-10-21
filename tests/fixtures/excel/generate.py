#!/usr/bin/env python3
"""Regenerate the committed Excel fixtures from a synthetic flat-ODF source.

Requires LibreOffice (`soffice`) only when regenerating; tests use the
committed outputs. Output bytes include LibreOffice timestamps, but cell
content is deterministic. LibreOffice cannot write .xlsb; the native tests
build their .xlsb fixtures in code (src-tauri/src/tests/excel.rs).

    python3 tests/fixtures/excel/generate.py
"""
import pathlib
import shutil
import subprocess
import sys
import tempfile
from xml.sax.saxutils import escape

HERE = pathlib.Path(__file__).resolve().parent


def cell(value=None, kind=None, style=None, formula=None):
    attrs = []
    if style:
        attrs.append(f'table:style-name="{style}"')
    if formula:
        attrs.append(f'table:formula="{escape(formula)}"')
    text = ""
    if kind == "float":
        attrs += ['office:value-type="float"', f'office:value="{value}"']
        text = str(value)
    elif kind == "string":
        attrs.append('office:value-type="string"')
        text = value
    elif kind == "bool":
        attrs += ['office:value-type="boolean"', f'office:boolean-value="{str(value).lower()}"']
        text = str(value).upper()
    elif kind in ("date", "datetime"):
        attrs += ['office:value-type="date"', f'office:date-value="{value}"']
        text = value
    if kind is None and formula is None:
        return "<table:table-cell/>"
    body = f"<text:p>{escape(text)}</text:p>" if text != "" else ""
    return f"<table:table-cell {' '.join(attrs)}>{body}</table:table-cell>"


def row(*cells):
    return "<table:table-row>" + "".join(cells) + "</table:table-row>"


def s(v):
    return cell(v, "string")


def f(v):
    return cell(v, "float")


DATA = [
    row(*map(s, ["id", "name", "amount", "ratio", "when", "stamp", "active", "code", "twice", "check"])),
    row(f(1), s("Alice"), f(12.5), f(0.1), cell("2024-02-29", "date", "ce_date"),
        cell("2024-02-29T13:45:30", "datetime", "ce_dt"), cell(True, "bool"), s("00123"),
        cell(25, "float", formula="of:=[.C2]*2"), cell(formula="of:=1/0")),
    row(f(2), s("Bob"), f(3), f(0.30000000000000004), cell("1999-12-31", "date", "ce_date"),
        cell("1900-03-01T06:00:00", "datetime", "ce_dt"), cell(False, "bool"), s("007"),
        cell(6, "float", formula="of:=[.C3]*2"), s("ok")),
    row(f(3), cell(), f(-7.25), f(1e-07), cell("2000-01-01", "date", "ce_date"),
        cell("2000-01-01T00:00:00.5", "datetime", "ce_dt"), cell(True, "bool"), s(""),
        cell(-14.5, "float", formula="of:=[.C4]*2"), f(42)),
    row(f(4), s("Émile ✓ <b>"), f(1234567.891), f(2.5), cell("1900-01-01", "date", "ce_date"),
        cell("2038-01-19T03:14:07", "datetime", "ce_dt"), cell(False, "bool"), s("A-1"),
        cell(2469135.782, "float", formula="of:=[.C5]*2"), cell(True, "bool")),
]
OFFSET = [
    row(s("Quarterly report")),
    row(),
    row(s("region"), s("q1")),
    row(s("north"), f(10)),
    row(s("south"), f(20)),
]
HIDDEN = [row(s("secret"), f(1))]
DUPES = [
    row(s("x"), s("x"), cell(), s("y")),
    row(f(1), f(2), f(3), f(4)),
    row(f(5), f(6), f(7), f(8)),
]


def table(name, rows, style=None):
    attr = f' table:style-name="{style}"' if style else ""
    return f'<table:table table:name="{name}"{attr}>' + "".join(rows) + "</table:table>"


FODS = f"""<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:number="urn:oasis:names:tc:opendocument:xmlns:datastyle:1.0"
 xmlns:of="urn:oasis:names:tc:opendocument:xmlns:of:1.2"
 office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.spreadsheet">
<office:automatic-styles>
 <number:date-style style:name="N_DATE"><number:year number:style="long"/><number:text>-</number:text><number:month number:style="long"/><number:text>-</number:text><number:day number:style="long"/></number:date-style>
 <number:date-style style:name="N_DT"><number:year number:style="long"/><number:text>-</number:text><number:month number:style="long"/><number:text>-</number:text><number:day number:style="long"/><number:text> </number:text><number:hours number:style="long"/><number:text>:</number:text><number:minutes number:style="long"/><number:text>:</number:text><number:seconds number:style="long" number:decimal-places="3"/></number:date-style>
 <style:style style:name="ce_date" style:family="table-cell" style:data-style-name="N_DATE"/>
 <style:style style:name="ce_dt" style:family="table-cell" style:data-style-name="N_DT"/>
 <style:style style:name="ta_hidden" style:family="table"><style:table-properties table:display="false"/></style:style>
</office:automatic-styles>
<office:body><office:spreadsheet>
{table("Data", DATA)}{table("Offset", OFFSET)}{table("Hidden", HIDDEN, "ta_hidden")}{table("Empty", [row()])}{table("Dupes", DUPES)}
</office:spreadsheet></office:body></office:document>
"""

FILTERS = {
    "xlsx": "xlsx:Calc MS Excel 2007 XML",
    "xls": "xls:MS Excel 97",
    "xlsm": "xlsm:Calc MS Excel 2007 VBA XML",
}


def main():
    soffice = shutil.which("soffice")
    if not soffice:
        sys.exit("soffice (LibreOffice) is required to regenerate fixtures")
    source = HERE / "workbook.fods"
    source.write_text(FODS, encoding="utf-8")
    with tempfile.TemporaryDirectory() as tmp:
        profile = pathlib.Path(tmp, "profile").as_uri()
        for ext, flt in FILTERS.items():
            subprocess.run(
                [soffice, "--headless", f"-env:UserInstallation={profile}", "--convert-to", flt,
                 "--outdir", tmp, str(source)],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120,
            )
            shutil.copyfile(pathlib.Path(tmp, f"workbook.{ext}"), HERE / f"workbook.{ext}")
    print("wrote", ", ".join(f"workbook.{e}" for e in FILTERS))


if __name__ == "__main__":
    main()

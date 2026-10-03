#!/usr/bin/env python3
"""Regenerates scanned_field_report.pdf for tests/document_ml_runtime.rs.

That test exercises the real (ONNX/PDFium) Docling OCR path through the
public `DocumentConverter`, so the fixture must be a PDF with *no* text
layer at all — only a rendered raster of legible English text — so the
pipeline has no choice but to run OCR to recover it.

Reproducibility requirements (deliberately minimal, nothing new to install):
  - Python 3 stdlib (zip/xml-free; this writes raw PDF syntax directly) plus
    `PdfBuilder` imported from the sibling `generate_basic_pdf_fixtures.py`
    in this same folder — not duplicated.
  - Pillow (already installed in this environment) purely to rasterize text;
    `ImageFont.load_default(size=...)` uses Pillow's own bundled font, so no
    system font path or extra font package is required.

Run from this directory: `python3 generate_document_ml_fixtures.py`.
"""
import pathlib
import sys
import zlib

from PIL import Image, ImageDraw, ImageFont

HERE = pathlib.Path(__file__).parent

# Reuse the hand-rolled PDF writer from generate_basic_pdf_fixtures.py rather
# than duplicating it; script-directory import, since both generators live
# (and are run) side by side in this fixtures folder.
sys.path.insert(0, str(HERE))
from generate_basic_pdf_fixtures import PdfBuilder  # noqa: E402

# Kept in sync with the phrases asserted in tests/document_ml_runtime.rs.
PHRASES = ["FIELD REPORT", "COASTAL OBSERVATIONS"]


def render_scanned_page() -> Image.Image:
    """Renders the fixture's two phrases as one grayscale raster, large and
    high-contrast enough for OCR regardless of the pipeline's render DPI."""
    width, height = 1400, 360
    image = Image.new("L", (width, height), color=255)
    draw = ImageDraw.Draw(image)
    font = ImageFont.load_default(size=72)
    draw.text((30, 30), PHRASES[0], fill=0, font=font)
    draw.text((30, 190), PHRASES[1], fill=0, font=font)
    return image


def build_scanned_pdf(image: Image.Image) -> bytes:
    """One page containing only the raster (no font resource, no text-showing
    operators), so the PDF genuinely carries no text layer to fall back on."""
    width, height = image.size
    raw = zlib.compress(image.tobytes())  # 8-bit DeviceGray samples, row-major.

    pdf = PdfBuilder()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    page = pdf.reserve()
    xobject = pdf.add_stream(
        f"/Type /XObject /Subtype /Image /Width {width} /Height {height} "
        "/ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
        raw,
    )
    content = f"q {width} 0 0 {height} 0 0 cm /Im1 Do Q\n".encode()
    contents = pdf.add_stream("", content)

    pdf.set(
        page,
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 {width} {height}] "
        f"/Resources << /XObject << /Im1 {xobject} 0 R >> >> "
        f"/Contents {contents} 0 R >>".encode(),
    )
    pdf.set(pages, f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())
    return pdf.build()


def main() -> None:
    image = render_scanned_page()
    pdf_bytes = build_scanned_pdf(image)
    out_path = HERE / "scanned_field_report.pdf"
    out_path.write_bytes(pdf_bytes)
    print(out_path.name, len(pdf_bytes), "bytes", image.size)


if __name__ == "__main__":
    main()

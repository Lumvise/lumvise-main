#!/usr/bin/env python3
"""Regenerates the hand-built PDF fixtures used by tests/basic_document_pdf.rs.

Each PDF is assembled as plain, uncompressed PDF syntax (same style as the
existing report.pdf fixture) via a tiny builder below, so the bytes stay
readable and auditable. Run from this directory:
`python3 generate_basic_pdf_fixtures.py`.
"""
import pathlib

HERE = pathlib.Path(__file__).parent
TINY_JPEG = (HERE / "tiny.jpg").read_bytes()


class PdfBuilder:
    """Assembles a minimal, uncompressed single-xref-table PDF."""

    def __init__(self):
        self.objects: list[bytes] = []

    def reserve(self) -> int:
        self.objects.append(b"")
        return len(self.objects)

    def set(self, obj_num: int, body: bytes) -> None:
        self.objects[obj_num - 1] = body

    def add(self, body: bytes) -> int:
        self.objects.append(body)
        return len(self.objects)

    def add_dict(self, fields: str) -> int:
        return self.add(f"<< {fields} >>".encode())

    def add_stream(self, fields: str, data: bytes) -> int:
        body = f"<< {fields} /Length {len(data)} >>\nstream\n".encode() + data + b"\nendstream"
        return self.add(body)

    def build(self) -> bytes:
        out = bytearray(b"%PDF-1.4\n")
        offsets = [0] * (len(self.objects) + 1)
        for i, body in enumerate(self.objects, start=1):
            offsets[i] = len(out)
            out += f"{i} 0 obj\n".encode()
            out += body
            out += b"\nendobj\n"
        xref_offset = len(out)
        count = len(self.objects) + 1
        out += f"xref\n0 {count}\n".encode()
        out += b"0000000000 65535 f \n"
        for i in range(1, count):
            out += f"{offsets[i]:010d} 00000 n \n".encode()
        out += f"trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF".encode()
        return bytes(out)


FONT_FIELDS = "/Type /Font /Subtype /Type1 /BaseFont /Helvetica"


def build_text_and_image() -> bytes:
    """Page with text, a table, a clipping path (graphical-path regression),
    and an embedded small raw-raster image (DeviceRGB, no filter)."""
    pdf = PdfBuilder()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    page = pdf.reserve()
    font = pdf.add_dict(FONT_FIELDS)

    # 2x2 raw RGB raster, uncompressed (no /Filter): red, green, blue, white.
    image_data = bytes([255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
    image = pdf.add_stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 "
        "/ColorSpace /DeviceRGB /BitsPerComponent 8",
        image_data,
    )

    content = (
        b"q 50 50 100 100 re W n 0 0 1 rg Q\n"
        b"BT /F1 16 Tf 30 250 Td (Field report) Tj ET\n"
        b"BT /F1 12 Tf 30 230 Td (Coastal observations.) Tj ET\n"
        b"BT /F1 12 Tf 30 210 Td (Region Count) Tj ET\n"
        b"BT /F1 12 Tf 30 190 Td (Coast 12) Tj ET\n"
        b"q 40 0 0 40 30 100 cm /Im1 Do Q\n"
    )
    contents = pdf.add_stream("", content)

    pdf.set(
        page,
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 420 300] "
        f"/Resources << /Font << /F1 {font} 0 R >> /XObject << /Im1 {image} 0 R >> >> "
        f"/Contents {contents} 0 R >>".encode(),
    )
    pdf.set(pages, f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())
    return pdf.build()


def build_image_only() -> bytes:
    """Single page with an embedded image but no text content at all —
    exercises the scanned-document OCR error even though pixels exist."""
    pdf = PdfBuilder()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    page = pdf.reserve()

    image_data = bytes([10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120])
    image = pdf.add_stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 "
        "/ColorSpace /DeviceRGB /BitsPerComponent 8",
        image_data,
    )
    content = b"q 40 0 0 40 30 100 cm /Im1 Do Q\n"
    contents = pdf.add_stream("", content)

    pdf.set(
        page,
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 420 300] "
        f"/Resources << /XObject << /Im1 {image} 0 R >> >> "
        f"/Contents {contents} 0 R >>".encode(),
    )
    pdf.set(pages, f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())
    return pdf.build()


def build_jpeg_embed() -> bytes:
    """Page with text plus a JPEG (DCTDecode) XObject, which must be
    preserved byte-for-byte rather than re-encoded."""
    pdf = PdfBuilder()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    page = pdf.reserve()
    font = pdf.add_dict(FONT_FIELDS)

    image = pdf.add_stream(
        "/Type /XObject /Subtype /Image /Width 4 /Height 4 "
        "/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        TINY_JPEG,
    )
    content = (
        b"BT /F1 16 Tf 30 250 Td (Coastal chart) Tj ET\n"
        b"q 40 0 0 40 30 100 cm /Im1 Do Q\n"
    )
    contents = pdf.add_stream("", content)

    pdf.set(
        page,
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 420 300] "
        f"/Resources << /Font << /F1 {font} 0 R >> /XObject << /Im1 {image} 0 R >> >> "
        f"/Contents {contents} 0 R >>".encode(),
    )
    pdf.set(pages, f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())
    return pdf.build()


def build_unsupported_image() -> bytes:
    """Page with text plus an image in an encoding this fallback does not
    support (DeviceCMYK raw samples) — text extraction must still succeed,
    with the image dropped and a warning recorded instead of a hard error."""
    pdf = PdfBuilder()
    catalog = pdf.reserve()
    pages = pdf.reserve()
    page = pdf.reserve()
    font = pdf.add_dict(FONT_FIELDS)

    # 2x2 raw CMYK raster (4 bytes/pixel) — unsupported color space.
    image_data = bytes([0, 0, 0, 255] * 4)
    image = pdf.add_stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 "
        "/ColorSpace /DeviceCMYK /BitsPerComponent 8",
        image_data,
    )
    content = (
        b"BT /F1 16 Tf 30 250 Td (Field report) Tj ET\n"
        b"BT /F1 12 Tf 30 230 Td (Coastal observations.) Tj ET\n"
        b"q 40 0 0 40 30 100 cm /Im1 Do Q\n"
    )
    contents = pdf.add_stream("", content)

    pdf.set(
        page,
        f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 420 300] "
        f"/Resources << /Font << /F1 {font} 0 R >> /XObject << /Im1 {image} 0 R >> >> "
        f"/Contents {contents} 0 R >>".encode(),
    )
    pdf.set(pages, f"<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>".encode())
    pdf.set(catalog, f"<< /Type /Catalog /Pages {pages} 0 R >>".encode())
    return pdf.build()


def main() -> None:
    fixtures = {
        "basic_text_image.pdf": build_text_and_image(),
        "image_only.pdf": build_image_only(),
        "jpeg_embed.pdf": build_jpeg_embed(),
        "unsupported_image.pdf": build_unsupported_image(),
    }
    for name, data in fixtures.items():
        (HERE / name).write_bytes(data)
        print(name, len(data), "bytes")


if __name__ == "__main__":
    main()

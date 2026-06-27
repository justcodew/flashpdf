"""Smoke test for the optional `render` feature (page.get_pixmap via PDFium).

Skips entirely when:
  - the sample PDF is missing (test_data/2604.11578v1.pdf), OR
  - PDFium binary is not reachable (no PDFIUM_PATH env and no ./pdfium-bin/).

The first skip is a checkout-level guard; the second reflects the runtime
nature of PDFium — the wheel may be built with `--features render` but the
binary has to be supplied separately. See docs/RENDERING.md.
"""
from __future__ import annotations

import io
import os
import shutil
from pathlib import Path

import pytest

import flashpdf

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
SAMPLE = PROJECT / "test_data" / "2604.11578v1.pdf"


def _pdfium_available() -> bool:
    """Mirror of render.rs::pdfium_available(): env var or ./pdfium-bin/."""
    if os.environ.get("PDFIUM_PATH"):
        return True
    return (PROJECT / "pdfium-bin").exists() and any(
        (PROJECT / "pdfium-bin").iterdir()
    )


# Check 1: the feature must be compiled in. We detect this by trying a call
# against a real file and looking for NotImplementedError.
def _render_feature_enabled() -> bool:
    if not SAMPLE.exists():
        return False
    try:
        with flashpdf.open(str(SAMPLE)) as doc:
            doc[0].get_pixmap(dpi=72)
    except NotImplementedError:
        return False
    except Exception:
        # Any other error means the feature IS compiled in; the failure is
        # downstream (e.g. missing PDFium binary). Treat as "enabled".
        return True
    return True


pytestmark = pytest.mark.skipif(
    not SAMPLE.exists(),
    reason="sample PDF not available in this checkout",
)


def test_get_pixmap_raises_not_implemented_without_feature():
    """If the wheel was built WITHOUT --features render, get_pixmap() must
    raise NotImplementedError with a helpful message rather than silently
    missing. This is the API-discoverability contract."""
    if _render_feature_enabled():
        pytest.skip("render feature is enabled — NotImplemented check is N/A")
    with flashpdf.open(str(SAMPLE)) as doc:
        with pytest.raises(NotImplementedError, match="render"):
            doc[0].get_pixmap(dpi=72)


def test_get_pixmap_returns_png_bytes():
    """End-to-end: open PDF, render first page, get back PNG bytes."""
    if not _render_feature_enabled():
        pytest.skip("render feature not compiled into this wheel")
    if not _pdfium_available():
        pytest.skip("PDFium binary not available — see docs/RENDERING.md")

    with flashpdf.open(str(SAMPLE)) as doc:
        page = doc[0]
        png = page.get_pixmap(dpi=72)

    # PNG magic header
    assert png[:8] == b"\x89PNG\r\n\x1a\n", "PNG magic header missing"
    assert len(png) > 500, "PNG suspiciously small"


def test_get_pixmap_writes_to_output_path(tmp_path: Path):
    """`output=` argument writes the same bytes to a file."""
    if not _render_feature_enabled():
        pytest.skip("render feature not compiled into this wheel")
    if not _pdfium_available():
        pytest.skip("PDFium binary not available — see docs/RENDERING.md")

    out = tmp_path / "page0.png"
    with flashpdf.open(str(SAMPLE)) as doc:
        png_bytes = doc[0].get_pixmap(dpi=72, output=str(out))

    assert out.exists()
    on_disk = out.read_bytes()
    assert on_disk == png_bytes


def test_get_pixmap_higher_dpi_produces_larger_png():
    """300 DPI should produce a larger PNG than 72 DPI for the same page."""
    if not _render_feature_enabled():
        pytest.skip("render feature not compiled into this wheel")
    if not _pdfium_available():
        pytest.skip("PDFium binary not available — see docs/RENDERING.md")

    with flashpdf.open(str(SAMPLE)) as doc:
        page = doc[0]
        low = page.get_pixmap(dpi=72)
        high = page.get_pixmap(dpi=300)
    assert len(high) > len(low), "300 DPI should yield a larger PNG than 72 DPI"

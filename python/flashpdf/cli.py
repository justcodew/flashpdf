"""flashpdf command-line interface.

Wraps `flashpdf.open()` / `extract()` / `get_toc()` for ad-hoc inspection
and quick batch extraction. Implemented in Python (click) rather than Rust
(clap) to keep the surface thin — every command delegates to the existing
pyo3 API.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Iterable

import click

from . import __version__, open as open_pdf, extract


# ─── helpers ──────────────────────────────────────────────────────────────────


def _parse_pages(spec: str | None, total: int) -> list[int] | None:
    """Parse a 0-based page spec like "0,3,5-8" into a sorted unique list.

    Returns None when `spec` is None (means "all pages").
    """
    if spec is None:
        return None
    out: set[int] = set()
    for part in spec.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            lo, _, hi = part.partition("-")
            lo_i = int(lo) if lo else 0
            hi_i = int(hi) if hi else total - 1
            out.update(range(max(0, lo_i), min(total, hi_i + 1)))
        else:
            i = int(part)
            if 0 <= i < total:
                out.add(i)
    return sorted(out)


def _emit_json_or_text(
    data: object, output: Path | None, *, indent: int = 2
) -> None:
    """Write `data` as JSON to file or stdout. Strings pass through as text."""
    if isinstance(data, (str, bytes)):
        text = data if isinstance(data, str) else data.decode("utf-8", "replace")
        if output is None:
            click.echo(text)
        else:
            output.write_text(text, encoding="utf-8")
        return
    text = json.dumps(data, ensure_ascii=False, indent=indent, default=str)
    if output is None:
        click.echo(text)
    else:
        output.write_text(text, encoding="utf-8")
        click.echo(f"wrote {output}", err=True)


def _expand_inputs(paths: Iterable[str]) -> list[Path]:
    """Expand shell glob-like patterns into a sorted Path list.

    click already passes argv-expanded args on POSIX, but Windows shells
    don't expand globs — handle it here so `flashpdf extract *.pdf` works
    cross-platform.
    """
    import glob

    out: list[Path] = []
    for p in paths:
        matches = sorted(glob.glob(p)) or [p]
        out.extend(Path(m) for m in matches)
    return out


# ─── commands ─────────────────────────────────────────────────────────────────


@click.group(
    help="""\
flashpdf — fast PDF text and image extraction.

Examples:
  flashpdf extract paper.pdf                  # plain text to stdout
  flashpdf extract paper.pdf --mode dict      # fitz-style JSON
  flashpdf extract paper.pdf --pages 0,1,5-8  # subset
  flashpdf extract *.pdf --output-dir out/    # batch
  flashpdf info paper.pdf                     # metadata + page stats
  flashpdf toc paper.pdf                      # outline / TOC
"""
)
@click.version_option(__version__, prog_name="flashpdf")
def main() -> None:
    """Entry point — see subcommands."""


@main.command("extract")
@click.argument("paths", nargs=-1, required=True)
@click.option(
    "--mode",
    type=click.Choice(["text", "dict", "blocks"]),
    default="text",
    show_default=True,
    help="Output format. 'text' = plain text per page; 'dict' / 'blocks' = fitz JSON.",
)
@click.option(
    "--pages",
    type=str,
    default=None,
    help="0-based page subset, e.g. '0,1,5-8'. Default: all pages.",
)
@click.option(
    "--no-images",
    is_flag=True,
    default=False,
    help="Skip image extraction (faster, smaller dict output).",
)
@click.option(
    "--output-dir",
    type=click.Path(file_okay=False, dir_okay=True, writable=True),
    default=None,
    help="Write one .txt/.json file per input PDF into this directory.",
)
@click.option(
    "--indent",
    type=int,
    default=2,
    show_default=True,
    help="JSON indent (only used with --mode dict/blocks).",
)
def extract_cmd(
    paths: tuple[str, ...],
    mode: str,
    pages: str | None,
    no_images: bool,
    output_dir: str | None,
    indent: int,
) -> None:
    """Extract text (or fitz-style JSON) from one or more PDFs."""
    inputs = _expand_inputs(paths)
    if not inputs:
        raise click.UsageError("no input PDFs matched")

    if output_dir is not None:
        os.makedirs(output_dir, exist_ok=True)

    outdir = Path(output_dir) if output_dir else None
    suffix = ".txt" if mode == "text" else ".json"

    for path in inputs:
        if not path.is_file():
            click.echo(f"skip {path}: not a file", err=True)
            continue
        try:
            doc = open_pdf(str(path), include_images=not no_images)
        except Exception as exc:  # noqa: BLE001 — surface as CLI error
            click.echo(f"error opening {path}: {exc}", err=True)
            continue

        page_idx = _parse_pages(pages, len(doc))
        iterable = page_idx if page_idx is not None else range(len(doc))

        if mode == "text":
            chunks = [doc[i].get_text("text") for i in iterable]
            text = "\n\u00ad\n".join(chunks)  # soft-break separator between pages
            target = (outdir / (path.stem + suffix)) if outdir else None
            _emit_json_or_text(text, target)
        else:
            payload = {
                "source": str(path),
                "page_count": len(doc),
                "pages": [
                    {"page": i, "result": doc[i].get_text(mode)}
                    for i in iterable
                ],
            }
            target = (outdir / (path.stem + suffix)) if outdir else None
            _emit_json_or_text(payload, target, indent=indent)

        doc.close()


@main.command("info")
@click.argument("path", type=click.Path(exists=True, dir_okay=False))
@click.option(
    "--metadata/--no-metadata",
    default=True,
    show_default=True,
    help="Include the metadata dict.",
)
@click.option(
    "--per-page/--summary-only",
    default=False,
    show_default=True,
    help="Emit one entry per page (is_scanned, block/image counts).",
)
def info_cmd(path: str, metadata: bool, per_page: bool) -> None:
    """Print document metadata and per-page stats."""
    try:
        doc = open_pdf(str(path), include_images=True)
    except Exception as exc:  # noqa: BLE001
        raise click.ClickException(f"failed to open {path}: {exc}") from exc

    out: dict[str, object] = {
        "source": path,
        "page_count": len(doc),
        "pdf_version": doc.metadata.get("format"),
    }
    if metadata:
        out["metadata"] = doc.metadata

    if per_page:
        pages = []
        for i in range(len(doc)):
            p = doc[i]
            d = p.get_text("dict")
            text_blocks = sum(1 for b in d["blocks"] if b.get("type", 0) == 0)
            image_blocks = sum(1 for b in d["blocks"] if b.get("type", 0) == 1)
            pages.append(
                {
                    "page": i,
                    "is_scanned": p.is_scanned,
                    "text_blocks": text_blocks,
                    "image_blocks": image_blocks,
                }
            )
        out["pages"] = pages
    else:
        scanned = sum(1 for i in range(len(doc)) if doc[i].is_scanned)
        out["scanned_pages"] = scanned

    _emit_json_or_text(out, None, indent=2)
    doc.close()


@main.command("toc")
@click.argument("path", type=click.Path(exists=True, dir_okay=False))
@click.option(
    "--simple/--rich",
    default=True,
    show_default=True,
    help="Simple mode emits [level, title, page] tuples; rich emits full dicts.",
)
@click.option(
    "--indent",
    type=str,
    default="  ",
    show_default=True,
    help="Indent string for tree-style simple output (empty = JSON lines).",
)
def toc_cmd(path: str, simple: bool, indent: str) -> None:
    """Print the document outline (table of contents)."""
    try:
        doc = open_pdf(str(path), include_images=False)
    except Exception as exc:  # noqa: BLE001
        raise click.ClickException(f"failed to open {path}: {exc}") from exc

    toc = doc.get_toc(simple=simple)
    if not toc:
        click.echo("(no outline)", err=True)
        doc.close()
        return

    if simple and indent:
        for level, title, page in toc:
            click.echo(f"{indent * (level - 1)}{title}\t[p.{page}]")
    else:
        _emit_json_or_text(toc, None, indent=2)

    doc.close()


if __name__ == "__main__":
    main()

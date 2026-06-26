"""Smoke tests for the flashpdf CLI wrapper."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
from click.testing import CliRunner

from flashpdf.cli import main

HERE = Path(__file__).resolve().parent
PROJECT = HERE.parent
SAMPLE = PROJECT / "test_data" / "2604.11578v1.pdf"

pytestmark = pytest.mark.skipif(
    not SAMPLE.exists(),
    reason="sample PDF not available in this checkout",
)


def test_cli_help_lists_subcommands():
    runner = CliRunner()
    res = runner.invoke(main, ["--help"])
    assert res.exit_code == 0, res.output
    assert "extract" in res.output
    assert "info" in res.output
    assert "toc" in res.output


def test_cli_version():
    runner = CliRunner()
    res = runner.invoke(main, ["--version"])
    assert res.exit_code == 0
    assert "flashpdf" in res.output


def test_cli_info_emits_metadata_and_page_count():
    runner = CliRunner()
    res = runner.invoke(main, ["info", str(SAMPLE)])
    assert res.exit_code == 0, res.output
    payload = json.loads(res.output)
    assert payload["page_count"] == 14
    assert payload["pdf_version"] == "PDF 1.7"
    assert "metadata" in payload
    assert "title" in payload["metadata"]


def test_cli_info_per_page_flags():
    runner = CliRunner()
    res = runner.invoke(main, ["info", str(SAMPLE), "--per-page"])
    assert res.exit_code == 0, res.output
    payload = json.loads(res.output)
    assert "pages" in payload
    assert len(payload["pages"]) == 14
    assert payload["pages"][0]["page"] == 0


def test_cli_toc_simple_matches_known_structure():
    runner = CliRunner()
    res = runner.invoke(main, ["toc", str(SAMPLE)])
    assert res.exit_code == 0, res.output
    lines = [ln for ln in res.output.splitlines() if ln.strip()]
    # Title page (level 1) appears first; level-2 entries are indented.
    assert lines[0].startswith("Minimizing classical")
    assert lines[1].startswith("  Abstract")


def test_cli_toc_rich_json():
    runner = CliRunner()
    res = runner.invoke(main, ["toc", str(SAMPLE), "--rich"])
    assert res.exit_code == 0, res.output
    payload = json.loads(res.output)
    assert isinstance(payload, list)
    assert payload[0]["level"] == 1
    assert "title" in payload[0]


def test_cli_extract_text_default_mode():
    runner = CliRunner()
    res = runner.invoke(main, ["extract", str(SAMPLE), "--pages", "0"])
    assert res.exit_code == 0, res.output
    assert "Minimizing classical resources" in res.output


def test_cli_extract_dict_to_output_dir(tmp_path):
    runner = CliRunner()
    res = runner.invoke(
        main,
        [
            "extract",
            str(SAMPLE),
            "--mode",
            "dict",
            "--pages",
            "0",
            "--output-dir",
            str(tmp_path),
        ],
    )
    assert res.exit_code == 0, res.output
    out_file = tmp_path / "2604.11578v1.json"
    assert out_file.exists()
    payload = json.loads(out_file.read_text())
    assert payload["page_count"] == 14
    assert len(payload["pages"]) == 1
    assert "blocks" in payload["pages"][0]["result"]


def test_cli_extract_pages_range_subset():
    runner = CliRunner()
    res = runner.invoke(
        main, ["extract", str(SAMPLE), "--pages", "0-1", "--mode", "dict"]
    )
    assert res.exit_code == 0, res.output
    payload = json.loads(res.output)
    assert len(payload["pages"]) == 2
    assert [p["page"] for p in payload["pages"]] == [0, 1]


def test_cli_extract_missing_file_skips_cleanly():
    """Batch mode tolerates missing files — no crash, exit 0."""
    runner = CliRunner()
    res = runner.invoke(main, ["extract", "/no/such/file.pdf"])
    assert res.exit_code == 0
    # The skip is reported on stderr.
    assert "skip" in res.output or "not a file" in res.output


def test_cli_entry_point_installed():
    """`flashpdf` script is registered via [project.scripts]."""
    if not os.environ.get("VIRTUAL_ENV"):
        pytest.skip("needs an active venv with the package installed")
    exe = shutil.which("flashpdf")
    if exe is None:
        pytest.skip("flashpdf script not on PATH")
    out = subprocess.check_output([exe, "--version"], text=True)
    assert "flashpdf" in out


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))

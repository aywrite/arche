# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2022-2026 Andrew Wright

"""Tests for the bench trailer script.

A cargo shim stands in for the build and writes a fake engine where the real
one would go, so what is under test is that the script builds when it has to,
reads the bench's last line, and prints the trailer in the shape the hook
accepts.
"""

import os
import subprocess
import sys
from pathlib import Path

import check_trailers
import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "bench_trailer.sh"

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="runs a shell script, which windows cannot"
)


def fake_engine(path, nodes):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "#!/usr/bin/env bash\n"
        "echo 'bench depth 7 hash 16MB positions 18'\n"
        "echo 'total 1 2 3'\n"
        f"echo '{nodes} nodes 12345678 nps'\n"
    )
    path.chmod(0o755)


def run(cwd, env):
    return subprocess.run(
        [str(SCRIPT)],
        cwd=cwd,
        env={"PATH": env.get("PATH", "/usr/bin:/bin"), **env},
        check=False,
        capture_output=True,
        text=True,
    )


def test_it_builds_and_prints_the_trailer(tmp_path):
    shims = tmp_path / "shims"
    shims.mkdir()
    cargo = shims / "cargo"
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        "echo built >> cargo.log\n"
        "mkdir -p target/release\n"
        "printf '#!/usr/bin/env bash\\necho 42847751 nodes 1 nps\\n' > target/release/arche\n"
        "chmod +x target/release/arche\n"
    )
    cargo.chmod(0o755)
    result = run(tmp_path, {"PATH": f"{shims}:/usr/bin:/bin"})
    assert result.returncode == 0, result.stderr
    assert result.stdout == "Bench: 42847751\n"
    assert (tmp_path / "cargo.log").read_text() == "built\n"


def test_the_build_is_read_from_where_cargo_was_told_to_put_it(tmp_path):
    # CARGO_TARGET_DIR moves the build; reading target/ regardless would
    # bench whatever stale binary sat there
    shims = tmp_path / "shims"
    shims.mkdir()
    cargo = shims / "cargo"
    cargo.write_text(
        "#!/usr/bin/env bash\n"
        'mkdir -p "$CARGO_TARGET_DIR/release"\n'
        "printf '#!/usr/bin/env bash\\necho 777 nodes 1 nps\\n'"
        ' > "$CARGO_TARGET_DIR/release/arche"\n'
        'chmod +x "$CARGO_TARGET_DIR/release/arche"\n'
    )
    cargo.chmod(0o755)
    fake_engine(tmp_path / "target" / "release" / "arche", 1)
    result = run(
        tmp_path,
        {
            "PATH": f"{shims}:/usr/bin:/bin",
            "CARGO_TARGET_DIR": str(tmp_path / "elsewhere"),
        },
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout == "Bench: 777\n"


def test_a_binary_named_in_the_environment_is_used_without_building(tmp_path):
    engine = tmp_path / "elsewhere" / "arche"
    fake_engine(engine, 777)
    result = run(tmp_path, {"ARCHE": str(engine), "PATH": "/usr/bin:/bin"})
    assert result.returncode == 0, result.stderr
    assert result.stdout == "Bench: 777\n"
    assert not (tmp_path / "target").exists()


def test_the_trailer_passes_the_hook(tmp_path):
    engine = tmp_path / "arche"
    fake_engine(engine, 42847751)
    result = run(tmp_path, {"ARCHE": str(engine), "PATH": "/usr/bin:/bin"})
    message = f"fix(search): Do a thing\n\n{result.stdout}"
    assert check_trailers.problems(message) == []


def test_a_missing_engine_fails_rather_than_printing_an_empty_trailer(tmp_path):
    result = run(
        tmp_path, {"ARCHE": str(tmp_path / "nowhere"), "PATH": "/usr/bin:/bin"}
    )
    assert result.returncode != 0
    assert "Bench:" not in result.stdout
    assert os.path.basename(SCRIPT) in result.stderr or result.stderr

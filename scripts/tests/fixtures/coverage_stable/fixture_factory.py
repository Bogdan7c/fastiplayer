"""Малый LLVM JSON3.1 fixture с реальными segments/function/region границами."""

from __future__ import annotations

from pathlib import Path


def _summary(covered: int, count: int) -> dict[str, int | float]:
    return {"count": count, "covered": covered, "percent": 100.0 * covered / count if count else 0.0}


def _file_summary(lines: tuple[int, int], functions: tuple[int, int], regions: tuple[int, int]):
    return {
        "lines": _summary(*lines),
        "functions": _summary(*functions),
        "instantiations": _summary(*functions),
        "regions": {**_summary(*regions), "notcovered": regions[1] - regions[0]},
        "branches": {**_summary(0, 0), "notcovered": 0},
        "mcdc": {**_summary(0, 0), "notcovered": 0},
    }


def build_report(
    repo_root: Path,
    *,
    run: int = 1,
    add_file: bool = False,
) -> dict:
    """Строит full export; run меняет hits, но никогда source topology."""

    if run not in (1, 2, 3):
        raise ValueError("fixture run должен быть 1..3")
    alpha_path = str(repo_root / "crates/alpha/src/lib.rs")
    shell_path = str(repo_root / "crates/shell/src/lib.rs")
    dependency_path = "/registry/dependency/src/lib.rs"
    variable_two_hits = (0, 2, 2)[run - 1]
    variable_one_hit = (3, 0, 0)[run - 1]
    closure_count = (0, 1, 0)[run - 1]
    alpha_segments = [
        [1, 1, 1, True, True, False],
        [1, 5, 0, False, False, False],
        [3, 1, variable_two_hits, True, True, False],
        [3, 5, 0, False, False, False],
        [5, 1, 0, True, True, False],
        [5, 4, 0, True, True, True],
        [6, 1, 0, False, True, False],
        [6, 4, variable_one_hit, True, True, False],
        [7, 1, variable_one_hit, True, False, False],
        [9, 1, 0, False, False, False],
    ]
    alpha_regions = [
        [10, 1, 12, 2, 1, 0, 0, 0],
        [11, 3, 11, 8, 0, 0, 0, 0],
        [12, 3, 12, 8, 1, 0, 1, 1],
    ]
    functions = [
        {
            "name": "alpha::<u8>",
            "count": 1,
            "filenames": [alpha_path, dependency_path],
            "regions": alpha_regions,
            "branches": [],
            "mcdc_records": [],
        },
        {
            "name": "alpha::<u16>",
            "count": 0,
            "filenames": [alpha_path, dependency_path],
            "regions": [
                [10, 1, 12, 2, 0, 0, 0, 0],
                [11, 3, 11, 8, 1, 0, 0, 0],
                [12, 3, 12, 8, 0, 0, 1, 1],
            ],
            "branches": [],
            "mcdc_records": [],
        },
        {
            "name": "alpha::closure",
            "count": closure_count,
            "filenames": [alpha_path],
            "regions": [[20, 1, 20, 9, closure_count, 0, 0, 0]],
            "branches": [],
            "mcdc_records": [],
        },
        {
            "name": "shell::main",
            "count": 0,
            "filenames": [shell_path],
            "regions": [[2, 1, 2, 8, 0, 0, 0, 0]],
            "branches": [],
            "mcdc_records": [],
        },
        {
            "name": "dependency::ignored",
            "count": 999,
            "filenames": [dependency_path],
            "regions": [[1, 1, 1, 8, 999, 0, 0, 0]],
            "branches": [],
            "mcdc_records": [],
        },
    ]
    function_covered = 1 + int(closure_count > 0)
    region_covered = 2 + int(closure_count > 0)
    files = [
        {
            "filename": alpha_path,
            "segments": alpha_segments,
            "branches": [],
            "mcdc_records": [],
            "expansions": [],
            "summary": _file_summary((7, 8), (function_covered, 2), (region_covered, 3)),
        },
        {
            "filename": shell_path,
            "segments": [[2, 1, 0, True, True, False], [2, 8, 0, False, False, False]],
            "branches": [],
            "mcdc_records": [],
            "expansions": [],
            "summary": _file_summary((0, 1), (0, 1), (0, 1)),
        },
    ]
    if add_file:
        extra_path = str(repo_root / "crates/alpha/src/extra.rs")
        files.append(
            {
                "filename": extra_path,
                "segments": [[1, 1, 1, True, True, False], [1, 4, 0, False, False, False]],
                "branches": [],
                "mcdc_records": [],
                "expansions": [],
                "summary": _file_summary((1, 1), (0, 0), (0, 0)),
            }
        )
    return {
        "type": "llvm.coverage.json.export",
        "version": "3.1.0",
        "cargo_llvm_cov": {"version": "0.8.7", "manifest_path": str(repo_root / "Cargo.toml")},
        "data": [
            {
                "files": files,
                "functions": functions,
                "totals": _file_summary(
                    (7 + int(add_file), 9 + int(add_file)),
                    (function_covered, 3),
                    (region_covered, 4),
                ),
            }
        ],
    }

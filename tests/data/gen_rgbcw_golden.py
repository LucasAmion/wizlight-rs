#!/usr/bin/env python3
"""Record what `pywizlight` computes, so the Rust port can be held to it.

The RGB+CW conversion in `src/protocol/colour.rs` is a port of `pywizlight`'s
`rgbcw.py`. A port is only worth anything if it agrees with what it was ported
from, so this script drives the original over a fixed grid of inputs and writes
the answers to `rgbcw_golden.json`, which `tests/colour.rs` then checks the port
against.

`pywizlight` is *not* a dependency of this crate and is not installed by
anything here: the table is committed, and regenerating it is a deliberate act.
Run it with a checkout alongside this one, or point `--pywizlight` at one:

    python3 tests/data/gen_rgbcw_golden.py --pywizlight ../pywizlight

Only `rgbcw.py` and `vec.py` are loaded, by path rather than as a package, so
that none of `pywizlight`'s own dependencies have to be installed.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
from itertools import product
from pathlib import Path
from types import ModuleType

# The inputs, chosen rather than swept. A dense grid would re-walk the same
# few branches thousands of times and make the table unreviewable; what
# catches a bad port is covering each branch, each boundary between them, and
# each of the six sextants the hue decomposition treats separately. The dense
# checking that remains worth doing — that nothing is out of range, and that
# the round trip is stable — is in `tests/colour.rs`, which sweeps a million
# colours without needing any of them recorded here.

# One coarse pass round the hue circle, so that a mistake confined to one
# sextant cannot hide between the boundary cases below.
HUE_CIRCLE = [step * 15.0 for step in range(24)]
HUE_CIRCLE_SATURATIONS = (25.0, 50.0, 100.0)

# The hues where the decomposition changes shape: exactly on a primary, where
# it uses one component; a hair either side, where it uses two; the bisectors,
# where both coefficients are 1; and the wrap, which is arithmetic rather than
# geometry.
HUE_EDGES = (
    0.0,
    0.1,
    59.9,
    60.0,
    119.9,
    120.0,
    120.1,
    180.0,
    239.9,
    240.0,
    240.1,
    300.0,
    359.9,
    360.0,
    719.5,
)
# Zero, the epsilon band just above it, both sides of the discontinuity at 50,
# and the top of the range. 49.5 is not redundant with 49.9: it is far enough
# below the step that moving the step shows up as a difference of several
# channel values rather than of one, which the tolerance would swallow.
SATURATIONS = (0.0, 0.001, 45.0, 49.5, 49.9, 50.0, 50.1, 75.0, 99.5, 100.0)

# For `rgb2rgbcw`, which recovers saturation from the length of the triple:
# the corners of the cube, the axes (a pure hue at every length, including the
# lengths that straddle the discontinuity), the greys (no hue at all, and the
# short ones fall in the epsilon band), and two-channel mixes either side of a
# bisector.
RGB_AXES = (1, 2, 63, 64, 127, 128, 129, 192, 254, 255)
RGB_GREYS = (1, 2, 3, 4, 64, 128, 255)
RGB_MIXES = (1, 64, 127, 128, 192, 254, 255)
RGB_ASSORTED = (
    (255, 200, 170),
    (255, 210, 210),
    (170, 200, 255),
    (12, 34, 56),
    (200, 100, 50),
    (50, 100, 200),
    (100, 200, 50),
    (3, 2, 1),
)

# `cw` is read back from a bulb, which may report more than the 128 the
# algorithm itself will ever emit, so the input range is the full byte, and
# both sides of the `cw == 1` branch are covered.
READBACK_RGB = (
    (0, 0, 0),
    (255, 255, 255),
    (255, 0, 0),
    (0, 255, 0),
    (0, 0, 255),
    (255, 255, 0),
    (0, 255, 255),
    (255, 0, 255),
    (255, 127, 0),
    (0, 128, 255),
    (64, 32, 0),
    (2, 0, 0),
)
READBACK_CW = (0, 1, 64, 127, 128, 129, 255)


def load(pywizlight: Path) -> ModuleType:
    """Import `rgbcw.py` and its `vec.py` by path, skipping the package."""
    package = pywizlight / "pywizlight"
    modules = {}
    for name in ("vec", "rgbcw"):
        spec = importlib.util.spec_from_file_location(
            f"pywizlight.{name}", package / f"{name}.py"
        )
        if spec is None or spec.loader is None:
            raise SystemExit(f"cannot load {package / f'{name}.py'}")
        module = importlib.util.module_from_spec(spec)
        # `rgbcw` does `from .vec import ...`, which needs `vec` registered
        # under its package name before `rgbcw` is executed.
        sys.modules[f"pywizlight.{name}"] = module
        spec.loader.exec_module(module)
        modules[name] = module
    return modules["rgbcw"]


def provenance(pywizlight: Path) -> dict:
    """Which `pywizlight` the numbers below came out of."""
    version = "unknown"
    version_file = pywizlight / "pywizlight" / "_version.py"
    if version_file.exists():
        for line in version_file.read_text().splitlines():
            if line.startswith("__version__"):
                version = line.split('"')[1]
    try:
        commit = subprocess.run(
            ["git", "-C", str(pywizlight), "rev-parse", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        commit = "unknown"
    return {"project": "pywizlight", "version": version, "commit": commit}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--pywizlight",
        type=Path,
        default=Path(__file__).resolve().parents[2].parent / "pywizlight",
        help="path to a pywizlight checkout (default: ../pywizlight)",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path(__file__).resolve().parent / "rgbcw_golden.json",
    )
    args = parser.parse_args()

    rgbcw = load(args.pywizlight)

    rgb_inputs = list(product((0, 255), repeat=3))
    for value in RGB_AXES:
        rgb_inputs += [(value, 0, 0), (0, value, 0), (0, 0, value)]
    rgb_inputs += [(value,) * 3 for value in RGB_GREYS]
    for value in RGB_MIXES:
        rgb_inputs += [(255, value, 0), (0, 255, value), (value, 0, 255)]
    rgb_inputs += list(RGB_ASSORTED)

    hs_inputs = list(product(HUE_CIRCLE, HUE_CIRCLE_SATURATIONS))
    hs_inputs += list(product(HUE_EDGES, SATURATIONS))

    def unique(values):
        """Deduplicate, keeping the order the reasons were written in."""
        return list(dict.fromkeys(values))

    rgb_to_rgbcw = []
    for rgb in unique(rgb_inputs):
        out_rgb, cw = rgbcw.rgb2rgbcw(rgb)
        rgb_to_rgbcw.append([list(rgb), [*out_rgb, cw]])

    hs_to_rgbcw = []
    for hue, saturation in unique(hs_inputs):
        out_rgb, cw = rgbcw.hs2rgbcw((hue, saturation))
        hs_to_rgbcw.append([[hue, saturation], [*out_rgb, cw]])

    rgbcw_to_hs = []
    for rgb in READBACK_RGB:
        for cw in READBACK_CW:
            hue, saturation = rgbcw.rgbcw2hs(rgb, cw)
            rgbcw_to_hs.append([[*rgb, cw], [hue, saturation]])

    table = {
        "format": (
            "Each entry is [input, output]. rgb_to_rgbcw: [[r,g,b],[r,g,b,cw]]. "
            "hs_to_rgbcw: [[hue_degrees,saturation_percent],[r,g,b,cw]]. "
            "rgbcw_to_hs: [[r,g,b,cw],[hue_degrees,saturation_percent]]."
        ),
        "generator": "tests/data/gen_rgbcw_golden.py",
        "source": provenance(args.pywizlight),
        "rgb_to_rgbcw": rgb_to_rgbcw,
        "hs_to_rgbcw": hs_to_rgbcw,
        "rgbcw_to_hs": rgbcw_to_hs,
    }

    with args.out.open("w") as handle:
        # One entry per line: the diff of a regenerated table is then readable.
        handle.write("{\n")
        keys = list(table)
        for index, key in enumerate(keys):
            value = table[key]
            tail = "" if index == len(keys) - 1 else ","
            if isinstance(value, list):
                handle.write(f"  {json.dumps(key)}: [\n")
                rows = ",\n".join(f"    {json.dumps(row)}" for row in value)
                handle.write(f"{rows}\n  ]{tail}\n")
            else:
                handle.write(f"  {json.dumps(key)}: {json.dumps(value)}{tail}\n")
        handle.write("}\n")

    print(
        f"{args.out}: {len(rgb_to_rgbcw)} rgb, {len(hs_to_rgbcw)} hs, "
        f"{len(rgbcw_to_hs)} readback"
    )


if __name__ == "__main__":
    main()

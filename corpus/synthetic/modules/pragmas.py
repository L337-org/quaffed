"""Line-bound pragmas: comments that tools read and that belong to the line they are on."""

import os  # noqa: F401
import sys  # noqa

value = compute()  # type: ignore[attr-defined]
other = compute()  # pragma: no cover

# fmt: off
matrix = [
    1,0,
    0,1,
]
# fmt: on


def legacy():  # noqa: C901
    return value  # pylint: disable=undefined-variable

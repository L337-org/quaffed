"""Every kind of statement, with the clauses each one takes."""

import os
import os.path as osp
from collections import OrderedDict, defaultdict as dd
from . import sibling
from ..parent import (
    first,
    second as other,
)
from .relative import *

counter = 0
total: int = 0
unset: str
a = b = c = 1
first, *rest = [1, 2, 3]
counter += 1
total -= 2
del a, b
type Pair = tuple[int, int]


@staticmethod
@property
@decorator.with_args(1, key="value")
def plain(positional, /, standard, *args, keyword_only, default=None, **kwargs) -> None:
    global counter
    counter = 0

    def inner():
        nonlocal standard
        standard = 1

    return inner


async def coroutine(session):
    async with session.lock() as held, session.other():
        async for item in session.items():
            await item
    return [x async for x in session.stream()]


def generator():
    yield
    yield 1
    received = yield 2
    yield from range(3)
    return received


class Base:
    pass


class Derived(Base, metaclass=type):
    """A docstring."""

    attribute: int = 0

    def method(self):
        return self.attribute


for index in range(10):
    if index == 1:
        continue
    elif index == 2:
        break
    else:
        pass
else:
    pass

while counter < 3:
    counter += 1
else:
    counter = 0

if counter:
    pass
elif total:
    pass
else:
    pass

with open("a") as handle, open("b"):
    pass

try:
    raise ValueError("bad") from None
except (ValueError, TypeError) as error:
    raise
except Exception:
    pass
else:
    pass
finally:
    pass

try:
    pass
except* OSError as group:
    pass

assert counter == 0, "message"
lambda: 0

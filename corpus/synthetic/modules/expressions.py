"""Every kind of expression, as a statement of its own or inside another."""

value = a and b or not c
walrus = (n := 10)
arithmetic = 1 + 2 - 3 * 4 / 5 // 6 % 7 ** 8 @ matrix
bits = ~x | y & z ^ w << 1 >> 2
signs = -x, +y
choice = left if condition else right
mapping = {"key": "value", **others}
members = {1, 2, 3}
listed = [1, *more, 3]
pair = (1, 2)
single = (1,)
empty = ()
squares = [n * n for n in range(10) if n % 2 if n > 2]
unique = {n for n in range(10)}
lookup = {k: v for k, v in pairs}
lazy = sum(n for n in range(10))
nested = [[i, j] for i in range(3) for j in range(i)]
compare = 1 < x <= 2 != y == z > w >= v is not None in container not in other is u
call = function(1, *args, key=2, **kwargs)
attribute = obj.attr.deeper
subscript = items[0]
sliced = items[1:2], items[::3], items[1:], items[:-1], items[...]
numbers = 0, 1_000, 0x1F, 0o17, 0b101, 1.5, 1e10, 3j
constants = True, False, None, ...
raw = b"bytes", rb"raw\bytes", B"upper"
anonymous = lambda x, *a, y=1, **k: x
starred = print(*args)

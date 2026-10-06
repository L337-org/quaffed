"""Type parameter syntax: every kind of parameter, bounds, constraints and defaults."""


def identity[T](value: T) -> T:
    return value


def bounded[T: int, U: (str, bytes)](first: T, second: U) -> tuple[T, U]:
    return first, second


class Container[T, *Ts, **P]:
    pass


class Defaults[T = int, *Ts = *tuple[int, ...], **P = [int, str]]:
    pass


type Alias[T] = list[T]
type Callback[**P, R] = Callable[P, R]

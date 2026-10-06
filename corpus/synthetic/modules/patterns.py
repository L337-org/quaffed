"""Every kind of match pattern."""

match command:
    case 1 | 2 | 3:
        pass
    case None | True | False:
        pass
    case "quit":
        pass
    case [first, second, *rest]:
        pass
    case (x, y) if x > y:
        pass
    case {"action": action, "target": target, **extra}:
        pass
    case Point(x=0, y=0):
        pass
    case Point(1, y=other):
        pass
    case [Point() as point, *_]:
        pass
    case constants.VALUE:
        pass
    case _:
        pass

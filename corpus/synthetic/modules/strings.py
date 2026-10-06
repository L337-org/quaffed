"""Every form of string: prefixes, quotes, concatenation, f-strings and t-strings."""

single = 'single'
double = "double"
triple_single = '''triple
single'''
triple_double = """triple
double"""
raw = r"raw \n stays"
escapes = "tab\there\nnewline é \N{BULLET} \x41"
joined = "implicit" 'concatenation' """across forms"""
data = b"bytes" rb"raw bytes"
name = "world"
greeting = f"hello {name}"
formatted = f"{value:>10} {value!r} {value=} {value:{width}.{precision}}"
nested = f"outer {f'inner {name}'} done"
multiline = f"""
    {name}
    {name!s:^20}
"""
raw_f = rf"raw {name} \n"
braces = f"{{literal braces}} {name}"
template = t"hello {name}"
template_spec = t"{value:>{width}}"
joined_f = f"one {name}" "two" f"three {name}"

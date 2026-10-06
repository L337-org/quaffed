"""Indentation by tabs, by spaces, and by tabs in one block and spaces in another."""


def with_tabs():
	if True:
		return 1


def with_spaces():
    if True:
        return 2


class Both:
	def tabbed(self):
		return 3

	def also_tabbed(self):
		value = [
            1,
			2,
		]
		return value

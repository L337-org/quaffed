# Security Policy

## Reporting a vulnerability

If you believe you have found a security issue in quaffed, please open a private vulnerability
report via GitHub's
[security advisory flow](https://github.com/L337-org/quaffed/security/advisories/new) rather
than filing a public issue.  That keeps the discussion private until a fix is available.

## Supported versions

There is no release yet.  Once there is, fixes go into the latest release only.

## Scope

quaffed is an editor: it reads and rewrites files in the projects it is run over, and it runs
the script files those projects check in.  Anything that lets a script, a file being edited, or a
command-line argument cause quaff to write outside the files it was asked to edit, to write a
file it reported as unchanged, or to report an edit as made when it was not, is in scope.

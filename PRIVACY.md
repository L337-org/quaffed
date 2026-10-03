# Privacy Policy

**quaffed** does not collect, store, or transmit any data back to the author, and has no
telemetry or analytics of any kind.

**It makes no network connections at all.**  This is a design commitment rather than a
default: `quaff` reads and writes files on your own machine, and nothing else.  Language modules
and scripts are resolved locally, from the directory installed beside the binary or from your
project, and are never fetched.  There is no author-operated service, and nothing for your
code, your files or your usage to pass through.

*quaffed is pre-release, and only textual search is implemented.  This page states what it is
being built to do, and is part of the specification it will be held to.*

## What stays on your machine

- **The files it edits**, which it reads and writes in place.
- **A local journal** of what each run did - what was asked, what was in scope, what resolved and
  what was found or changed.  It is written per user, under your own state directory, and never
  leaves your machine.  Because it can contain text from the files a run searched, treat it with
  the same care as the project itself.  A project or a user can turn it off.

## When an AI agent runs it

quaffed is designed to be driven by AI agents.  Whatever `quaff` prints goes to the process that
ran it, and if that is an agent, the agent's client and the AI provider behind it have their own
privacy policies governing what is sent to the model.  This policy covers only the behaviour of
quaffed itself.

## Contact

Questions about this policy: open an issue at
<https://github.com/L337-org/quaffed/issues>.

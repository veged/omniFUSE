## `of skill`

Prints this agent-oriented manual. Symmetric to `--help` — every
invocation form that works for `help` also works for `skill`:

- `of --skill` ≡ `of skill` — top-level intro plus a subcommand index.
- `of mount git --skill` ≡ `of skill mount git` — section for one
  subcommand.
- Append `--for=<tool>` to attach tool-specific guidance. Currently
  supported tools: `claude`, `openai`, `langchain`. Unknown values
  fall back silently (no tail, no error).

Skill content is embedded in the binary and ships with each release.
The first line of the output is a comment with the `of` version so
you can detect a mismatch against a pinned binary.

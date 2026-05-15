---

## For Claude Code

The host expects you to access the mountpoint via `--add-dir`:

```bash
of mount git https://github.com/user/repo /tmp/repo &
claude --add-dir /tmp/repo
```

You can read, search, and edit files under `/tmp/repo` exactly as you
would any other workspace. Saved files become real git commits pushed
back to the remote.

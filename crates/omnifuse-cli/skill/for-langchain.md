---

## For LangChain

Use `ShellTool` (or any subprocess-based tool) with its working
directory set to the omniFUSE mountpoint:

```python
from langchain.tools import ShellTool
shell = ShellTool()
# Run with cwd pointing at the mounted directory
```

No custom adapter is required — the mount behaves like any local
filesystem.

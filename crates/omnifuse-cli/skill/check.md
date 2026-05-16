## `of check`

Verifies that the FUSE/WinFsp driver is installed and usable on the host.

Exits 0 on success; non-zero with installation instructions otherwise.

This is a host-level check — there is no equivalent over the mount. If
you are inside a sandbox without the host driver, mount the filesystem
on the host and bind/expose the mountpoint into the sandbox instead.

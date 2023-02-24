### `opendotgit`

A (work in progress) simple Rust tool to download and extract source code from misconfigured open .git directories.

For the blind case where a .git directory does not list its contents:
[x] references are fetched
[x] object file names are collected from references
[ ] pack files are parsed for more object files
[ ] object files are fetched

### `opendotgit`

A simple Rust tool to download and extract source code from misconfigured open `.git` directories.

#### Installation

Binary releases will be available following the first stable release. For now,
you may need to setup cargo to install opendotgit. Run the following command:

```sh
cargo install --git https://github.com/lavafroth/opendotgit
```

#### Usage

```
opendotgit [OPTIONS] <URL> <OUTPUT>
```

#### Positional arguments

- _URL_: URL of the .git directory
- _OUTPUT_: Directory to output the results

#### Options

```
  -j, --jobs <JOBS>        Number of asynchronous jobs to spawn [default: 8]
  -v, --verbose...         Turn debugging information on
  -r, --retries <RETRIES>  Number of times to retry a failed request [default: 3]
  -t, --timeout <SECONDS>  [default: 10]
  -h, --help               Print help
  -V, --version            Print version
```

#### A note on directory exposure

Opendotgit will try its best to dump the source code from a `.git` directory regardless of whether
it prohibits listing subdirectories. As long as the respective files like `.git/HEAD` can be accessed,
opendotgit will switch to the blind strategy to infer from the known files and dump the repository
that way.

For the blind case where a .git directory does not list its contents:
- [x] references are fetched
- [x] object file names are collected from references
- [ ] pack files are parsed for more object files
- [ ] object files are fetched

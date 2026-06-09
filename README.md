# prism-bootstrap

A small CLI that turns a MultiMC/Prism instance into a runnable `java ...` command -
with no launcher installed. It reads an instance (`mmc-pack.json` +
`patches/*.json`), downloads every library, native, asset, and the client jar it
references, and emits the exact command (and an argv file) that boots the game.

Since we build off the instance file definitions (only json so far), we have to fetch most
of the required libraries & components from remote sources (such as mc's assets themselves).
Therefore, in basically all cases, an internet connection is required.

## Usage

```
prism-bootstrap <instance-dir> --platform <token> [options]
```

- `<instance-dir>` - directory holding `mmc-pack.json` and `patches/`; also where
  outputs are written.
- `--platform <token>` - the *target* platform (never auto-detected from the
  host), one of: `linux`, `linux-arm64`, `linux-arm32`, `linux-ppc64le`,
  `freebsd`, `osx`, `osx-arm64`, `windows`, `windows-arm64`, `windows-x86`. Omit
  it to stop after printing the merged-profile summary.

Common options: `--meta-url <url>` (resolve pack-only instances, see below),
`--xms`/`--xmx` (heap), `--headless`, `--jobs <n>`, `--java <path>`,
`--game-dir <path>`, `--emit <path>`, `--no-verify`, `--dry-run`, and the
dummy-auth flags (`--username`/`--uuid`/`--access-token`/`--user-type`).

## Headless

The initial idea for the usage of this tool was setting up mc for headless ci instances.
Therefore the `--headless` option (when provided) generates a simple launch.env with
the environment variables required when running with `xvfb-run` (and some other niceties).

#### If you want to test this: 
Other system packages that are required for running with `xvfb-run` (on debian at least):
- `xvfb`
- `libgl1-mesa-dri`
- `xauth`
- `libegl1` 
- `libegl-mesa0`

Then, after bootstrapping your instance, go into .minecraft and run:

#### For java 9+
`bash -c 'env $(grep -v "^\\s*#" ../launch.env | xargs) xvfb-run -n 99 -f ./xvfb.auth -s "-screen 0 854x480x24" "$(head -1 ../launch.argv)" @<(tail -n +2 ../launch.argv)' > headless.log.txt 2>&1 &`

#### For java -8
`bash -c 'mapfile -t argv < ../launch.argv; env $(grep -v "^\s*#" ../launch.env | xargs) xvfb-run -n 99 -f ./xvfb.auth -s "-screen 0 854x480x24" "${argv[0]}" "${argv[@]:1}"' > headless.log 2>&1 &`

> bash -c in case of other shells

> grep before env to allow comments

And then to check if it worked (after a short while):

`DISPLAY=:99 XAUTHORITY=./xvfb.auth scrot -o test.png`


If you want to get a recording of the startup, you can use the script at `record_start.sh` - it needs these packages (debian):
- ffmpeg
- fonts-dejavu-core
- python3-xlib
- xdotool

## How it works

The typical pipeline runs one module per stage:

1. **load** (`load.rs`) - read `mmc-pack.json` and each `patches/<uid>.json`, in
   manifest array order.
2. **meta** (`meta.rs`) - for any component with no local patch, fetch its
   version file from `--meta-url` (see *Meta resolution*); without `--meta-url`,
   a gap is a hard error.
3. **merge** (`merge.rs`) - fold the components into one `Profile`: libraries
   accumulate, `mainClass`/`mainJar`/`assetIndex` are last-wins, and
   args/tweakers/traits/agents accumulate. The fold order is the manifest array
   order (the patch `order` field is informational).
4. **platform + rules** (`platform.rs`, `rules.rs`) - expand `--platform` into a
   context and evaluate each library/arg's `rules`, accepting both the MMC
   arch-in-name and classic Mojang dialects plus the `features` gate.
5. **resolve** (`resolve.rs`) - classify each kept library into a role (classpath
   / maven-file / native-extract), compute its maven path under `libraries/`,
   attach its url (kept verbatim - never reconstructed), and dedupe.
6. **download + assets** (`download.rs`, `assets.rs`) - fetch every artifact from remote, verify against SHA-1 & size; the asset
   index and its objects flow through the same path. (`--dry-run` skips this.)
7. **natives** (`natives.rs`) - extract legacy i.e. LWJGL2) native jars into
   `natives/`; modern natives ride the classpath and need no extraction.
8. **java + assemble + emit** (`java.rs`, `assemble.rs`, `emit.rs`) - pick a JDK
   (by `compatibleJavaMajors`/`javaVersion`, or `--java`), build the classpath
   and substitute both argument forms, inject the heap, then write `launch.argv`
   and print the command.

## Instance format

Each component (`patches/<uid>.json`) carries `libraries`, an optional
`mainClass`/`mainJar`/`assetIndex`, one of two argument forms
(`minecraftArguments` string, or modern `arguments {game, jvm}`), and operator
lists (`+libraries`/`-libraries`, `+jvmArgs`/`+tweakers`/`+traits`/`+agents`).
Library entries cover plain jars, bare `name`+`url` repos, empty-hash (skip
verify), no-url `MMC-hint: local` (must be pre-placed under `libraries/`), and
natives (modern per-OS classifier jars vs legacy `classifiers`+`natives`+
`extract`). 

For some examples, check `example-files/`.

### Meta resolution

A "pack-only" instance lists its components by `uid`+`version` but ships no
`patches/`. Pass `--meta-url https://meta.prismlauncher.org/v1/` to fetch each
missing component's version file (`<base>/<uid>/<version>.json`, which is the same
patch schema) from the Prism meta server. It is opt-in: without `--meta-url`, a
missing patch is a hard error naming the file to provide.

## Output layout

```
<instance>/
  libraries/<group/path>/<artifact>-<ver>[-<classifier>].jar
  natives/                         extracted legacy natives
  assets/indexes/<id>.json
  assets/objects/<ab>/<hash>       [+ assets/virtual/<id>/... for legacy indexes]
  versions/<ver>/<ver>.jar         client jar
  minecraft/                      game working dir (created if absent)
  launch.argv                      emitted command, one token per line
  launch.env                       software-GL + silent-audio hints (only with --headless)
  alsoft.conf                      OpenAL Soft config (only with --headless)
  resolution.lock                  audit manifest of every resolved artifact
```

After assembling, we emit a `java ...` command for quick starts.

## Exit codes (yay, stuff broke...)

`0` ok - `2` bad platform - `3` no main class - `4` unsatisfied
requires/conflicts - `5` missing no-url local library - `6` download/SHA-1
failure - `7` IO/parse error - `8` missing component (needs `--meta-url`) - `9`
meta resolution failed.

## unit tests

```
cargo test               # fully offline
cargo test -- --ignored  # wet tests: needs network
```

# Launcher-free Prism/MMC Instance Resolver - Build Plan

A plan for a small, self-contained tool that reads **any** MultiMC/Prism
instance (`mmc-pack.json` + `patches/*.json`), downloads everything that
instance needs, and emits a runnable `java ...` command - with no launcher
installed.

The tool is built against the **format**, not against any one instance. Because
every Minecraft version and every mod loader is, inside a Prism instance,
expressed with the same primitives (components -> libraries + mainClass + args +
rules), covering the format generally means vanilla, Fabric, Quilt, Forge,
NeoForge, and exotic setups like GTNH/lwjgl3ify all fall out of the same code
path. Section 3 checks that claim explicitly.

> **Convention - examples are EXAMPLES.** Throughout this document, every
> concrete uid, version, URL, class name, SHA, or JSON snippet that mentions
> GTNH, lwjgl3ify, RFB, Forge 1.7.10, or `MainStartOnFirstThread` is an
> **illustration** drawn from one real instance used to make the logic concrete.
> None of it is a build target or a special case to hardcode. If a value below
> looks specific, read it as "e.g." - the implementation keys off the general
> schema in Section 6, and the specific instance simply happens to be one thing
> that schema can express.

---

## 1. Objective & end state

When the tool finishes successfully against a given instance, on disk we have:

1. A populated instance directory with every library, native, the client jar,
   and the full asset store that instance's patches reference.
2. A single launch command (string + argv array) that, run from the instance
   dir (under a virtual display, if headless), boots the game.

Each invocation builds one complete instance from scratch into a clean
directory - no shared store, no cross-run cache, network required every run.
Within a single run it verifies every download (SHA-1) and writes atomically, so
a transient failure can be retried safely; it neither relies on nor produces a
persistent cache.

## 2. Scope

In scope - the *engine* of whatever instance is given: its Minecraft version
libraries + client jar + assets, its loader's libraries, and the merged JVM
args, main class, tweakers/arguments, and natives.

Out of scope - the *content* and the *identity*:

- **Mods and configs are not in the version patches.** They live in
  `<instance>/.minecraft/mods/` and `config/` and are loaded by the mod loader
  at runtime, not on the JVM classpath. They come from the pack/overlay and are
  copied into the instance dir. The resolver builds the engine; the pack
  supplies the content. *(Example: the GTNH client zip ships lwjgl3ify and the
  pack's mods in `mods/` - those are copied in, not produced by this tool.)*
- Authentication. We inject dummy offline credentials (enough to reach the menu
  / load singleplayer). Online play is out of scope.

## 3. Generality: does this run *any* Prism instance?

**Yes - by construction - provided the resolver implements the general schema,
not just the subset one example happens to use.** The core architecture is
loader- and version-agnostic because it never branches on "is this Forge / is
this 1.7.10." It only ever: sorts components by `order`, accumulates their
libraries, picks the last `mainClass`, filters by `rules`, substitutes argument
templates, and joins a command. Every loader is just "more components."

What exercises which path:

| Instance type            | How it reduces to the same pipeline                          |
|--------------------------|--------------------------------------------------------------|
| Vanilla (any version)    | one `net.minecraft` component; libraries + assets + mainJar  |
| Fabric / Quilt           | + a loader component (its own `mainClass`, extra libraries)  |
| Forge (modern) / NeoForge| + loader component(s) with `mavenFiles` and modern `arguments`|
| Legacy Forge (1.7-1.12)  | + loader component with `+tweakers` (LaunchWrapper)          |
| GTNH / lwjgl3ify (example)| same as legacy Forge + an LWJGL3 swap + RFB early classpath  |

But the plan **as derived from a single 1.7.10 instance does not yet exercise
everything a general tool must implement.** These are the generalizations that
older example doesn't reveal, and each is required to cover arbitrary instances:

- **G1 - Modern argument format.** 1.13+ replaces the flat `minecraftArguments`
  string with a structured `arguments: { "game": [...], "jvm": [...] }`, where
  entries are plain strings *or* conditional `{ "rules": [...], "value": <str|[str]> }`.
  The resolver must accept **both** forms. (The example uses only the legacy
  string.)
- **G2 - JVM-arg placeholders.** Modern versions inject the classpath and
  natives path *through* `arguments.jvm`: `-cp ${classpath}`,
  `-Djava.library.path=${natives_directory}`, plus `${classpath_separator}`,
  `${library_directory}`, `${version_name}`, `${launcher_name}`,
  `${launcher_version}`. So the assembler substitutes these rather than always
  hardcoding `-cp`/`-Djava.library.path`. For legacy instances with no `jvm`
  args (the example), the launcher supplies `-cp`/`-Djava.library.path` itself.
- **G3 - Both rule dialects.** Support classic Mojang rules
  `os: { name, arch, version(regex) }` **and** `features: { is_demo_user,
  has_custom_resolution, ... }` (used to gate game args), in addition to the
  arch-encoded `os.name` tokens MMC normalizes to (Section 7). Feature flags
  default to false unless the launcher sets them.
- **G4 - `mavenFiles`.** Modern Forge/NeoForge list a `mavenFiles` array:
  artifacts downloaded into `libraries/` but **not** placed on the classpath
  (consumed by the module/transformer layer). Download like libraries; exclude
  from `-cp`.
- **G5 - Per-instance Java.** Read `compatibleJavaMajors` (Prism) or Mojang's
  `javaVersion.majorVersion` and select the matching JDK. "Run any instance"
  implies multiple JDKs provisioned (8 for =< 1.16, 17 for 1.18+, 21 for 1.20.5+).
  *(The example is notable precisely because it runs 1.7.10 on Java 17+ instead
  of 8 - an override, not the norm.)*
- **G6 - Legacy asset modes.** Honor `virtual` / `map_to_resources` in old asset
  indexes (materialize a readable tree). Modern indexes use the plain object
  store. (Build this into the asset pipeline regardless.)
- **G7 - Extensible traits & `+agents`.** Keep `+traits` handling table-driven
  (`FirstThreadOnMacOS`, `noapplet`, `legacyServices`, ...) and support `+agents`
  (Java agents) as pass-through `-javaagent` entries. Unknown traits -> warn,
  don't fail.
- **G8 - Library dedup / `-libraries`.** When components declare the same
  `group:artifact`, apply `-libraries` removals and dedupe (highest-order /
  last declaration wins) so the classpath has no duplicate or conflicting jars.

**Conclusion:** the architecture matches the goal; the *coverage* matches it
only once G1-G8 are implemented. The sections below fold these in so the schema
and modules are general, with the single-instance specifics clearly marked as
examples.

## 4. CLI, inputs & outputs

### 4.1 CLI

A single command does the whole job: resolve -> download -> emit. Minimum
invocation is the instance directory plus the target platform; everything else
defaults.

```
resolver <instance-dir> --platform <token> [options]
```

**Required**

- `<instance-dir>` (positional) - the directory containing `mmc-pack.json` and
  `patches/`. Also where outputs are written.
- `--platform <token>` - **required, chosen from a fixed list** (the tokens in
  Section 7: `linux`, `linux-arm64`, `linux-arm32`, `linux-ppc64le`, `freebsd`,
  `osx`, `osx-arm64`, `windows`, `windows-arm64`, `windows-x86`). No
  auto-detection - the caller states the target explicitly, and the tool rejects
  anything outside the list. The classic-rule fields (`os.name`/`arch`/`version`)
  are derived from the chosen token, not from the host.

**Options (all defaulted)**

| Option | Default | Meaning |
|--------|---------|---------|
| `--xms <size>` | `512m` | initial heap (the prior default) |
| `--xmx <size>` | `6144m` | max heap (mirrors the original instance dump; override freely) |
| `--headless` | off | adjust *internals* for headless (see 4.2); does **not** wrap in xvfb |
| `--jobs <n>` | `16` | parallel downloads |
| `--java <path>` | auto | JDK to put in the command; auto = resolve by `compatibleJavaMajors` from `PATH` |
| `--username <s>` | `CI` | dummy account name |
| `--uuid <s>` | `0...0` | dummy uuid |
| `--access-token <s>` | `0` | dummy token |
| `--user-type <s>` | `legacy` | `legacy` or `msa` |
| `--game-dir <path>` | `<instance>/.minecraft` | working dir / `${game_directory}` |
| `--emit <path>` | `<instance>/launch.argv` | where to write the argv |
| `--no-verify` | off | skip SHA-1 checks (faster re-runs) |
| `--dry-run` | off | resolve + emit the command, **skip** downloading |

`--xms`/`--xmx` are the only heap inputs; the tool injects `-Xms`/`-Xmx`
itself (these are never in the patches). Heap defaults stay as established
earlier and are overridable here.

### 4.2 Output contract

The tool always emits a **plain `java ...` command** - never an xvfb-wrapped one.
Display provisioning is the caller's responsibility (e.g. their own
`xvfb-run`). Concretely it writes `launch.argv` (one token per line) and prints
the same command to stdout.

`--headless` does **not** change that contract; it adjusts internals that make
the emitted command behave in a no-GPU/virtual-display context, without ever
adding xvfb:

- writes a companion `launch.env` with the software-GL hints
  (`LIBGL_ALWAYS_SOFTWARE=1`, `GALLIUM_DRIVER=llvmpipe`) for the caller to source;
- pins LWJGL's native-extract dir to a writable instance path
  (`-Dorg.lwjgl.system.SharedLibraryExtractDirectory=<instance>/lwjgl-natives`)
  to avoid `/tmp` surprises in containers;
- may set other no-display-friendly properties as needed.

The caller then does, e.g., `env $(cat launch.env | xargs) xvfb-run -a <command>`
- but the tool's job ends at the java command + env.

### 4.3 Inputs

- `mmc-pack.json` - the ordered list of components in the instance.
- `patches/<uid>.json` - one component definition per uid.
- Everything else arrives via the CLI (4.1).

**Output directory layout** (MultiMC-style, so the emitted command's
`--assetsDir` / classpath line are conventional):

```
<instance>/
  mmc-pack.json
  patches/<uid>.json ...                                  # inputs (read-only to us)
  libraries/<group/path>/<artifact>-<ver>[-<classifier>].jar   # maven layout
  natives/                         # extracted legacy natives (java.library.path)
  assets/
    indexes/<id>.json
    objects/<ab>/<hash>
    virtual/<id>/...                  # only if the index is virtual/legacy
  versions/<ver>/<ver>.jar          # client jar (a.k.a. mainJar)
  .minecraft/                       # gameDir: mods/, config/, saves/, options.txt
  launch.argv                       # emitted command, one token per line
```

## 5. The component model (the conceptual core)

Every patch is a **component** with a `uid`, `version`, and integer `order`. The
launcher folds them into one launch profile **in `mmc-pack.json` declaration
(array) order**:

> **Correction (verified against a real Prism launch command, 2026-06-07).** The
> effective order is the `components[]` **array order**, *not* a sort by the
> patch `order` field. In the example pack `org.lwjgl3` (order -1) precedes
> `net.minecraft` (order -2) in the array, and that is exactly the order Prism
> puts them on the classpath. The `order` field is informational; treat the array
> as authoritative. (The "sort by `order`" wording elsewhere in this doc predates
> this finding.)

| Field                | Merge behavior                                              |
|----------------------|-------------------------------------------------------------|
| `libraries`          | **accumulate** in component order -> classpath               |
| `+libraries`         | accumulate (additive operator form)                         |
| `-libraries`         | remove a previously-added library by name                   |
| `mavenFiles`         | accumulate; download but **not** on classpath (G4)          |
| `mainClass`          | **last-wins** (highest `order` that sets it)                |
| `mainJar`            | last-wins; goes on the classpath                            |
| `minecraftArguments` | legacy game-arg string (=<1.12)                              |
| `arguments`          | modern `{game, jvm}` (1.13+); rule-gated entries (G1)       |
| `+tweakers`          | accumulate -> emitted as `--tweakClass <name>` game args      |
| `+jvmArgs`           | accumulate -> JVM args (pre-tokenized)                        |
| `+traits` / `+agents`| accumulate -> launcher behavior / `-javaagent` (G7)          |
| `assetIndex`         | set by the Minecraft component                              |
| `requires`/`suggests`/`conflicts` | dependency metadata (pre-resolved; assert only) |

> **EXAMPLE - how the running example (GTNH/lwjgl3ify 1.7.10) merges.** Order:
> `net.minecraft` (-2) -> `org.lwjgl3` (-1) -> `forgepatches` (3) ->
> `net.minecraftforge` (5) -> `launchargs` (100). Effective `mainClass` =
> `com.gtnewhorizons.retrofuturabootstrap.MainStartOnFirstThread` (launchargs,
> order 100, overrides forge's `...Main` and vanilla's
> `net.minecraft.client.main.Main`). One tweaker:
> `cpw.mods.fml.common.launcher.FMLTweaker`. JVM args = the `--add-opens` /
> `-Djava.system.class.loader=...RfbSystemClassLoader` block from `forgepatches`.
> `FirstThreadOnMacOS` trait -> `-XstartOnFirstThread` on macOS only. This is one
> concrete folding of the general rules above - not a special path.

## 6. Schema reference

> All JSON snippets below use real values from the example instance to be
> concrete. The fields and shapes are the general schema; the values are "e.g."

### 6.1 `mmc-pack.json`

```jsonc
{
  "formatVersion": 1,
  "components": [
    {
      "uid": "net.minecraft",          // -> patches/net.minecraft.json
      "version": "1.7.10",             // EXAMPLE value
      "important": true,                // optional UI flag
      "dependencyOnly": true,           // optional: pulled in only as a dep
      "cachedName": "...", "cachedVersion": "...",
      "cachedRequires": [ ... ], "cachedVolatile": true   // optional cache fields
    }
  ]
}
```

Read `components[].uid` to know which patch files to load; the authoritative
merge order is each patch's own `order` (Section 5).

### 6.2 Component / patch file (`patches/<uid>.json`)

```jsonc
{
  "formatVersion": 1,
  "uid": "net.minecraftforge",         // EXAMPLE uid
  "name": "Forge-LWJGL3",              // display only
  "version": "10.13.4.1614",           // EXAMPLE
  "order": 5,                           // merge order (ascending)
  "type": "release", "releaseTime": "...",

  "mainClass": "...",                    // optional; last-wins
  "mainJar": { ... },                    // optional; see 6.5
  "assetIndex": { ... },                 // optional; see 6.5

  // game args - ONE of these forms:
  "minecraftArguments": "... ${...} ...",    // legacy string (=<1.12)
  "arguments": {                        // modern (1.13+), see 6.6 (G1)
    "game": [ "...", { "rules": [...], "value": "..." } ],
    "jvm":  [ "-cp", "${classpath}", { "rules": [...], "value": [ "..." ] } ]
  },

  "compatibleJavaMajors": [17, 21],    // optional JDK hint (G5)
  "compatibleJavaName": "...",

  "libraries":  [ ... ],   "+libraries": [ ... ],   "-libraries": [ "g:a:v" ],
  "mavenFiles": [ ... ],                 // download, not on classpath (G4)

  "+jvmArgs":  [ "...", "..." ],           // pre-tokenized argv elements
  "+tweakers": [ "fully.qualified.Tweaker" ],
  "+traits":   [ "FirstThreadOnMacOS" ],
  "+agents":   [ { "name": "g:a:v" } ],

  "requires":  [ { "uid": "net.minecraft", "equals": "1.7.10" } ],
  "suggests":  [ { "uid": "org.lwjgl3", "suggests": "3.4.2-..." } ],
  "conflicts": [ { "uid": "org.lwjgl" } ]
}
```

`+jvmArgs` entries are **already argv-tokenized** (e.g. `--add-opens` and
`java.base/java.io=ALL-UNNAMED` are two separate elements). Pass them through
verbatim.

### 6.3 Library entry - the variants to handle

**(a) Plain artifact** (the common case):
```jsonc
{ "name": "com.google.guava:guava:17.0",         // EXAMPLE
  "downloads": { "artifact": { "url": ".../guava-17.0.jar", "sha1": "...", "size": 2243036 } } }
```

**(b) Bare `name` + `url` base** (older MMC style, no `downloads` block):
```jsonc
{ "name": "org.example:lib:1.0", "url": "https://repo.example/maven/" }
```
-> derive the path from the maven `name` and append it to the `url` base.

**(c) Empty/missing hash** - download but skip verification:
```jsonc
{ "name": "lzma:lzma:0.0.1",                      // EXAMPLE
  "downloads": { "artifact": { "url": ".../lzma-0.0.1.jar", "sha1": "", "size": 0 } } }
```

**(d) No download URL** (e.g. `MMC-hint: "local"`, or any entry lacking a
resolvable `url`):
```jsonc
{ "name": "com.github.GTNewHorizons:lwjgl3ify:3.0.23:forgePatches",  // EXAMPLE
  "MMC-hint": "local" }
```
-> **Assume it is already present locally** at the maven path the `name` derives
to under `libraries/`. If it's there, use it; if it's missing, **fail** with a
clear message naming the expected path and coordinate. No external-source lookup,
no fetching - the tool does not guess where to get it. *(In the example this is
the RFB-carrying `forgePatches` jar, which the caller must drop into `libraries/`
beforehand; in a typical instance there are no such entries at all.)*

**(e) Natives - two mechanisms:**

*Modern (LWJGL3 / 1.13+): per-OS classifier is its own library, gated by `rules`,
classifier in the name -> **on the classpath**; the loader self-extracts.*
```jsonc
{ "name": "org.lwjgl:lwjgl-jemalloc-natives-linux:3.4.2-...",   // EXAMPLE
  "downloads": { "artifact": { "url": "...-natives-linux.jar", "sha1": "...", "size": ... } },
  "rules": [ { "action": "allow", "os": { "name": "linux" } } ] }
```

*Legacy (LWJGL2 / =<1.12): `classifiers` + `natives` os->classifier map +
`extract` -> **extracted** into `natives/`, then `-Djava.library.path`.*
```jsonc
{ "name": "net.java.jinput:jinput-platform:2.0.5",            // EXAMPLE
  "natives": { "linux": "natives-linux", "windows": "natives-windows-${arch}" },
  "downloads": { "classifiers": { "natives-linux": { "url": "...", "sha1": "...", "size": ... } } },
  "extract": { "exclude": [ "META-INF/" ] },
  "rules": [ ... ] }
```
Substitute `${arch}` (e.g. `32`/`64`) before classifier lookup.

### 6.4 Rules & platform tokens (G3)

`rules` is an ordered list; evaluate with Mojang semantics:

```
allowed(rules, ctx):              # ctx = { os_token, os_name, arch, version, features }
    if rules empty/absent: return true
    decision = false
    for r in rules:
        applies = true
        if "os" in r:       applies &= os_matches(r.os, ctx)      # name/arch/version-regex OR token
        if "features" in r: applies &= all(ctx.features.get(k)==v for k,v in r.features.items())
        if applies: decision = (r.action == "allow")
    return decision
```

Two `os` dialects to accept:
- **MMC tokens** (this instance): arch encoded in `os.name` -
  `linux`, `linux-arm32`, `linux-arm64`, `linux-ppc64le`, `freebsd`,
  `osx`, `osx-arm64`, `windows`, `windows-arm64`, `windows-x86`.
  Note `linux` = x86-64 specifically.
- **Classic Mojang**: separate `os.name` (`linux`/`osx`/`windows`),
  `os.arch` (`x86`/`x86_64`/`arm64`), `os.version` (regex). Map your platform to
  both representations so either dialect resolves.

`features` gate game args in modern `arguments.game` (e.g. demo mode, custom
resolution); default all to false for a normal launch.

### 6.5 `mainJar` & `assetIndex`

```jsonc
"mainJar": { "name": "com.mojang:minecraft:1.7.10:client",       // EXAMPLE
  "downloads": { "artifact": { "url": ".../client.jar", "sha1": "...", "size": ... } } }

"assetIndex": { "id": "1.7.10", "sha1": "...", "size": ..., "totalSize": ...,   // EXAMPLE id
  "url": "https://piston-meta.mojang.com/v1/packages/<sha1>/<id>.json" }
```
Asset pipeline: index -> `assets/indexes/<id>.json`; each `objects[name]={hash,size}`
-> `https://resources.download.minecraft.net/<hash[:2]>/<hash>` ->
`assets/objects/<hash[:2]>/<hash>`. Honor `virtual`/`map_to_resources` (G6).

### 6.6 Argument forms (G1/G2)

*Legacy* (`minecraftArguments`, string of space-separated tokens with `${...}`):
```
--username ${auth_player_name} --version ${version_name} --gameDir ${game_directory}
--assetsDir ${assets_root} --assetIndex ${assets_index_name} --uuid ${auth_uuid}
--accessToken ${auth_access_token} --userProperties ${user_properties} --userType ${user_type}
```

*Modern* (`arguments.game` / `arguments.jvm`): arrays of strings and
`{rules, value}` objects. Resolve each entry: include if `allowed(rules, ctx)`;
emit `value` (string or list) with placeholders substituted. `jvm` typically
carries `-cp ${classpath}`, `-Djava.library.path=${natives_directory}`, and
mac/`features`-gated extras.

Substitution variables: `${auth_player_name}`, `${version_name}`,
`${game_directory}`, `${assets_root}`, `${assets_index_name}`, `${auth_uuid}`,
`${auth_access_token}`, `${user_properties}`, `${user_type}`, `${classpath}`,
`${classpath_separator}`, `${natives_directory}`, `${library_directory}`,
`${launcher_name}`, `${launcher_version}`, `${game_assets}` (legacy virtual).

## 7. Platform resolution

The platform **token is supplied** by the required `--platform` argument (4.1),
not detected from the host - so a build can target any platform from anywhere.
One function expands the chosen token into the full `ctx`: the MMC token itself,
the classic `os.name` + `arch`, and an OS version string for regex rules (e.g.
`linux` -> name `linux`, arch `x86_64`; `osx-arm64` -> name `osx`, arch `arm64`).
Feature flags default false. Feed `ctx` into `allowed()`.

## 8. Module architecture

```
load        read mmc-pack.json + each patches/<uid>.json; keep array order (NOT `order`)
merge       fold components -> profile: libraries(+/-), mavenFiles, mainJar,
            assetIndex, mainClass(last-wins), minecraftArguments|arguments,
            +jvmArgs, +tweakers, +traits, +agents
filter      keep libs/args whose rules allow the target ctx; dedupe libraries
classify    each kept lib -> {plain-cp, modern-native-cp, legacy-native-extract,
            maven-file (no cp), no-url}; resolve url + maven local path
download    parallel fetch w/ retries + sha1 verify (skip empty hash);
            atomic writes (crash-safe within a run, no cross-run cache);
            no-url entries must already exist locally or FAIL
            (assets reuse this path)
natives     extract legacy natives -> natives/ (apply extract.exclude);
            with --headless skip input-device natives (e.g. jinput) - unused
assets      assetIndex -> indexes/ ; objects -> objects/<ab>/<hash> ; virtual tree
java        select JDK from compatibleJavaMajors / javaVersion (or --java)
assemble    classpath = [plain-cp + modern-native-cp] + mainJar
            jvmArgs   = (+jvmArgs) and/or (arguments.jvm substituted) + heap
                        + -Djava.library.path (if not already templated)
                        + headless internals (if --headless)
            gameArgs  = (minecraftArguments | arguments.game) substituted
                        + --tweakClass per tweaker
            mainClass = last-wins
emit        write launch.argv (one token per line) + print java command;
            if --headless also write launch.env  (never an xvfb wrapper)
```

Keep `download` generic over an artifact record `{url, sha1, size, local_path}`
so assets, libraries, natives, mavenFiles, the client jar, and no-url jars all
flow through one fetch+verify+write path (no-url entries skip the fetch and just
assert local presence).

## 9. Implementation phases (milestones)

1. **Loader + merge** - parse patches, produce the merged profile in memory
   (both arg forms, +/- operators, last-wins). Pure logic; unit-test against any
   instance's patches.
2. **Platform + rules** - both dialects + features; produce the included sets.
3. **Artifact resolver** - `name` -> maven path; attach url + local_path; handle
   classifier selection, `mavenFiles`, and no-url entries (assert-local-or-fail).
4. **Downloader** - parallel, retries, sha1 (empty-hash skip), atomic writes; wire
   in the asset pipeline (reuse the object-download routine).
5. **Natives** - extract legacy; confirm modern natives are classpath-only.
6. **Java select + assembler + emitter** - classpath join, arg substitution
   (both forms), tweakers, write `launch.argv` + wrapper.
7. **Smoke test** - run under `xvfb-run` + software GL; gate on
   "reached menu / no crash" (the headless boot-test runner).

## 10. Assembly algorithm (pseudocode)

```
profile = merge(components)        # mmc-pack array order, NOT sorted by `order`
ctx     = expand_platform(args.platform)        # required token -> ctx

cp, maven_only, natives_to_extract = [], [], []
for lib in dedupe(profile.libraries) + [profile.mainJar]:
    if not allowed(lib.rules, ctx): continue
    if lib.is_legacy_native:
        if args.headless and lib.is_input_device: continue   # skip unused (e.g. jinput)
        natives_to_extract.append(pick_classifier(lib, ctx))
    else:                     cp.append(lib.local_path)   # download() ran, or asserted local
for mf in profile.mavenFiles:               # downloaded, not classpathed
    if allowed(mf.rules, ctx): maven_only.append(mf.local_path)

subs = dummy_auth | dirs | {
    "classpath": PATHSEP.join(cp), "classpath_separator": PATHSEP,
    "natives_directory": f"{inst}/natives", "library_directory": f"{inst}/libraries",
    "version_name": ver, "assets_root": f"{inst}/assets", "assets_index_name": idx_id, ... }

jvm  = (profile.jvmArgs or []) + resolve_args(profile.arguments.jvm, ctx, subs)
jvm += [f"-Xms{args.xms}", f"-Xmx{args.xmx}"]            # defaults 512m / 6144m
if "${classpath}" not used in jvm: jvm += ["-cp", subs["classpath"]]   # legacy path
if "java.library.path" not set:    jvm += [f"-Djava.library.path={inst}/natives"]
if args.headless:                  jvm += headless_internals(inst)     # NOT xvfb

game = resolve_args_or_split(profile.minecraftArguments or profile.arguments.game, ctx, subs)
game += flatten(["--tweakClass", t] for t in profile.tweakers)

cmd  = [args.java or select_java(profile)] \
       + jvm + ([] if "-cp" in jvm else ["-cp", subs["classpath"]]) \
       + [profile.mainClass] + game
# emit: write launch.argv (+ launch.env if headless); print cmd. Never wrap in xvfb.
```

## 11. Final command shape

> **EXAMPLE output** for the GTNH/lwjgl3ify 1.7.10 instance (a legacy-args,
> launcher-supplies-classpath case). A 1.20 Fabric instance would instead carry
> `-cp ${classpath}` inside `arguments.jvm` and a Knot main class - same code,
> different emitted tokens.

```
<java17|21> \
  -Dfile.encoding=UTF-8 \
  -Djava.system.class.loader=com.gtnewhorizons.retrofuturabootstrap.RfbSystemClassLoader \
  --enable-native-access ALL-UNNAMED \
  --add-opens java.base/java.io=ALL-UNNAMED  ... (full +jvmArgs block) \
  -Xms512m -Xmx6144m \
  -Djava.library.path=<instance>/natives \
  -cp <lib1>:<lib2>:...:<instance>/versions/1.7.10/1.7.10.jar \
  com.gtnewhorizons.retrofuturabootstrap.MainStartOnFirstThread \
  --username CI --version 1.7.10 --gameDir <instance>/.minecraft \
  --assetsDir <instance>/assets --assetIndex 1.7.10 \
  --uuid 0...0 --accessToken 0 --userProperties {} --userType legacy \
  --tweakClass cpw.mods.fml.common.launcher.FMLTweaker
```

The tool emits exactly this `java` command (here `--xmx` left at its `6144m`
default). It does **not** wrap it: running it with working directory
`<instance>/.minecraft` is the caller's job, as is the virtual display. With
`--headless` the tool also writes `launch.env` with `LIBGL_ALWAYS_SOFTWARE=1`
`GALLIUM_DRIVER=llvmpipe` (and pins the LWJGL extract dir), so a caller does e.g.
`env $(xargs <launch.env) xvfb-run -a "$(cat launch.argv | tr '\n' ' ')"`.

## 12. Edge cases & gotchas

General logic (applies to any instance):

| # | Item | Handling |
|---|------|----------|
| 1 | Two argument forms | accept `minecraftArguments` *and* `arguments{game,jvm}` (G1) |
| 2 | Classpath/natives via args on modern versions | substitute `${classpath}`/`${natives_directory}`; don't double-add (G2) |
| 3 | Two rule dialects + features | MMC tokens and classic name/arch/version + feature gates (G3) |
| 4 | `mavenFiles` | download, exclude from classpath (G4) |
| 5 | Per-instance JDK | select from `compatibleJavaMajors`/`javaVersion`; provision 8/17/21 (G5) |
| 6 | Two natives mechanisms | modern -> classpath; legacy -> extract + `java.library.path` |
| 7 | Empty/missing `sha1` | download, skip verification |
| 8 | Duplicate libraries across components | apply `-libraries`, dedupe last-wins (G8) |
| 9 | Legacy virtual assets | honor `virtual`/`map_to_resources` (G6) |
| 10| `mainClass` is last-wins | never hardcode a class name |
| 11| `+jvmArgs` pre-tokenized | pass verbatim |
| 12| Mods/configs not in patches | copy from the pack into `.minecraft/` separately |
| 13| Reaching menu needs no valid auth | dummy `--accessToken 0` suffices |

Example-specific observations (true for the GTNH instance, not assumptions to bake in):

| # | Item | Note |
|---|------|------|
| E1| Entrypoint `MainStartOnFirstThread` | falls out of last-wins; not special-cased |
| E2| `forgePatches` is `MMC-hint: local` | no URL -> must be pre-placed in `libraries/`, else the tool fails (carries RFB) |
| E3| LWJGL3 libs are dated snapshots on GTNH nexus | pin to the patch URL; general rule "never resolve 'latest'" |
| E4| 1.7.10 on Java 17+ | an override; vanilla 1.7.10 would select Java 8 (G5) |
| E5| `linux` token = x86-64 | general token rule, surfaced by this instance |

## 13. Validation

Wrap the emitted command in the headless boot-test: launch under Xvfb + llvmpipe
with a hard timeout; PASS on a configured "menu reached" marker (or, v0, surviving
the window without a `crash-reports/` entry); FAIL on self-exit or crash report.
Later, swap the heuristic for a coremod that auto-joins a world and calls
`exitJava(0)`. This validation step is instance-agnostic.

## 14. Config knobs / open questions

- **Heap** - `--xms` (default `512m`), `--xmx` (default `6144m`); overridable.
- **Platform** - required `--platform`, validated against the fixed token list.
- **Feature flags**, **parallelism** (`--jobs`), **dummy account**, **JDK
  selection** (`--java` or auto from `compatibleJavaMajors`).
- **No-url libraries** - assumed already present under `libraries/`; the tool
  fails (naming the path) if missing. No external fetching. *(Example: the GTNH
  `forgePatches` jar must be dropped in beforehand.)*
- **Unused legacy natives** - with `--headless`, input-device natives (the
  jinput family) are skipped, since there's no input device; built-in skip-list,
  overridable via `--keep-natives`/`--skip-natives`. The extract path stays for
  non-headless or non-3ify instances that genuinely need LWJGL2 natives. *(For
  the 3ify example this empties the legacy-extract set, so `natives/` and
  `-Djava.library.path` aren't needed - but the capability remains general.)*
  The jinput jar stays on the classpath; absent its native, controller support
  is simply unavailable (fine headless). Don't skip a native an instance hard-depends on.
- **Layout: per-instance, clean every run (decided).** No shared `libraries/`
  or `assets/` store and no cross-run cache - each invocation downloads
  everything fresh into a clean instance directory. Implication: every run is
  network-bound and the egress allowlist (Section: repos) must be reachable each
  time; there is no offline mode beyond `--dry-run`.
- **Meta source** if an instance is missing patches: Prism resolves uids from
  `meta.prismlauncher.org`. Supporting that (fetch a uid's index -> version JSON ->
  synthesize a patch) is the one feature that would let the tool build an instance
  from scratch rather than only resolving an existing one. Out of scope for now,
  but it slots in at the `load` stage.

## 15. Notes for the implementer (read before coding)

These are the non-obvious correctness constraints and the validation strategy.
They matter more than the happy path.

### 15.1 Trust the explicit URL; never reconstruct snapshot URLs

When a library has `downloads.artifact.url` (or `classifiers[*].url`), use it
**verbatim**. Do **not** derive URLs from the maven `name` when one is given -
SNAPSHOT artifacts make derivation wrong: the `name` carries the *resolved*
version (`org.lwjgl:lwjgl:3.4.2-20260602.093430-9`) while the URL path uses the
*base* version dir with a *timestamped* filename
(`.../lwjgl/3.4.2-SNAPSHOT/lwjgl-3.4.2-20260602.093430-9.jar`). Only derive a URL
for the bare `name` + `url`-base variant (6.3b), and even then keep the local
path and the classpath entry produced by the **same** function so they can't
drift. General rule: pin exactly what the patch says; never resolve "latest".

### 15.2 Emit for the *target* platform, not the host

The classpath separator (`:` vs `;`) and path style follow `--platform`, not the
machine running the resolver. Running on Linux with `--platform windows` must
emit `;`-joined paths. Take the separator from the platform `ctx`
(`${classpath_separator}`), never from the host `os.pathsep`.

### 15.3 Fail-fast preflight + exit codes

Before downloading, validate and stop with a clear message + distinct nonzero
exit code on: unknown `--platform`; exactly-zero or genuinely-conflicting
`mainClass`; unsatisfied `requires`/`conflicts`; a no-url library missing from
`libraries/` (name the path); an unknown required schema field (warn, don't
silently drop). After downloading: any SHA-1 mismatch that survives retries is
fatal. Reserve exit 0 for "command emitted and (unless `--dry-run`) all artifacts
present and verified".

### 15.4 Determinism (for testability, not caching)

Even with no cache, keep output byte-stable given the same inputs: classpath
order = component `order` then declared order; dedupe must be **order-preserving**
(don't use an unordered set); arg substitution is pure. There's no cache to key,
but this is what makes the golden test below meaningful and makes failures
reproducible when debugging across instances.

### 15.5 Emit a resolution manifest (optional, audit/debug aid)

Optionally, alongside `launch.argv`, write a JSON `resolution.lock` listing every
resolved artifact: coordinate, source URL, sha1, local path, and role
(classpath / maven-file / native-extract / asset / skipped). With no cache it's
not a cache key, but it's still a cheap audit log, a diff target across
instances, and convenient input for the golden test. Lower priority than the
correctness items above.

### 15.6 Validation strategy

- **Unit**: merge (last-wins, `+/-libraries`, accumulate); `allowed()` for both
  rule dialects incl. the allow-then-disallow case (twitch on linux) and
  `features`; arg substitution for legacy string, modern `arguments`, and
  `{rules,value}` conditionals; maven-coordinate -> path; snapshot URL passthrough.
- **Golden**: capture the *real* Prism-emitted command for the example instance
  (via the wrapper-script trick - have Prism write its argv to a file), then
  assert the resolver's argv matches it modulo Prism-only extras the tool
  intentionally omits (heap, `-XX:HeapDumpPath`, `-Duser.language`, env). This is
  the single highest-value test: it proves the resolver reproduces a launcher's
  output on a real instance.
- **Smoke**: the headless boot-test (Section 13) as the end-to-end gate.

### 15.7 HTTP client details

Follow redirects (the ARM jinput natives are GitHub `raw` URLs that 302);
verify both size and sha1; retry with backoff; write atomically (`*.part` ->
rename) so an interrupted run leaves no half-file that a later run trusts.

### 15.8 Things the tool deliberately does NOT do

Add Prism-only JVM extras (`-XX:HeapDumpPath`, `-Duser.language`, GC flags) -
those are launcher preferences, not part of the instance; resolve `latest`
anything; fetch no-url libraries; provision a display or wrap in xvfb; install
JDKs (it only selects/points at one). Keeping these out of scope is what keeps
the output a faithful, minimal reproduction of the instance.

### 15.9 Optional, if the next implementer wants to widen scope

- `--overlay <dir>` to copy a pack's `mods/`+`config/` into the gameDir, bridging
  engine and content in one step.
- `meta.prismlauncher.org` resolution (Section 14) to build instances whose
  patches are incomplete - slots into the `load` stage.

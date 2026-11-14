# Spec format

A spec tells Lig what a command takes: its subcommands, options, arguments and where their
values come from. Lig reads [Fig](https://github.com/withfig/autocomplete)'s spec format, as
JSON or TOML; this page is the whole of what Lig uses.

Your specs go in `~/.config/lig/specs/` (`lig specs path` shows the folder), one file per
command, named after it: `deploy.toml` or `deploy.json`. Yours win over the built-in ones,
whole: start from a built-in with `lig specs new <command>` to change one. Check a file with
`lig specs check <command>`, which reports keys and values Lig doesn't understand, and try it
with `lig complete "deploy -e "`.

## An example

```toml
name = "deploy"
description = "Deploy the app"

# On every subcommand too.
[[options]]
name = ["-v", "--verbose"]
description = "Print each step"
isPersistent = true

# A value from a fixed list, written --env=… on the line.
[[options]]
name = ["-e", "--env"]
description = "Where to deploy"
requiresSeparator = true
args = { name = "env", suggestions = ["staging", "production"] }

# Not suggested once --force is on the line.
[[options]]
name = "--dry-run"
description = "Show what would change"
exclusiveOn = ["--force"]

[[options]]
name = "--force"
description = "Deploy even with local changes"
priority = 40

[[subcommands]]
name = ["logs", "log"]
description = "Show a service's logs"
# Live values: one line of the command's output each.
args = { name = "service", generators = { script = ["deploy", "list-services"], cache = { ttl = 10000 } } }

[[subcommands]]
name = "upload"
description = "Upload files"
args = { name = "file", isVariadic = true, template = "filepaths" }

[[subcommands]]
name = "run"
description = "Run a command on the server"
args = { name = "command", isCommand = true }

[[subcommands]]
name = "commit"
description = "Record a deploy note"
# The cursor lands between the quotes.
options = [{ name = "-m", description = "The note", insertValue = "-m '{cursor}'", args = { name = "note" } }]

# Shown next to the subcommands.
[[additionalSuggestions]]
name = "upload --all"
description = "Upload the whole build folder"
type = "shortcut"
```

Each key that takes a list (`name`, `options`, `args`, `suggestions`, `generators`…) also takes
a single value: `args = { … }` is one argument, `name = "--force"` one name.

## Commands and subcommands

The file is the command; each `[[subcommands]]` entry is one too, and nests the same way.

| Key | What it does |
| --- | --- |
| `name` | The command's name; a subcommand can have several (`["logs", "log"]`). |
| `subcommands` | Its subcommands. |
| `options` | Its options (see [Options](#options)). |
| `args` | Its positional arguments, in order (see [Arguments](#arguments)). |
| `additionalSuggestions` | More suggestions next to the subcommands, such as shortcuts (see [Suggestions](#suggestions)). |
| `loadSpec` | Complete as another command's spec: `loadSpec = "git"` (see [Reusing specs](#reusing-specs)). |
| `loadHelp` | Read the options from a command's `--help`, run when this subcommand is entered: `loadHelp = ["deploy", "logs"]`. |
| `parserDirectives` | How the line is read (see [Parser directives](#parser-directives)). |

Commands, subcommands, options and suggestions all take these:

| Key | What it does |
| --- | --- |
| `description` | Shown next to it. |
| `displayName` | Shown instead of the name (the name is still what's typed and matched). |
| `insertValue` | Inserted instead of the name. `{cursor}` marks where the cursor goes after. |
| `priority` | 0–100, default 50: higher comes first, among what matches equally well. |
| `hidden` | `true`: only suggested once its whole name is typed. |
| `icon` | A different icon, by name: `lig icons` lists them (`git-branch`, `package`, `docker`…). |

## Options

| Key | What it does |
| --- | --- |
| `name` | Its names: `["-e", "--env"]`. |
| `args` | The value(s) it takes (see [Arguments](#arguments)); without, it's a flag. |
| `isPersistent` | Also an option of every subcommand below. |
| `isRepeatable` | Suggested again after it's been used (`-v -v`); `true` or a number. |
| `requiresSeparator` | The value is joined to it: `true` for `--env=staging`, or the separator itself (`":"`). `requiresEquals = true` is the same as `true`. |
| `exclusiveOn` | Options it can't be used with: once one of them is on the line, it's no longer suggested. |

## Arguments

| Key | What it does |
| --- | --- |
| `name` | What it is (`file`, `service`), shown while nothing else is. |
| `isOptional` | It can be left out: the options show where it would go, and an option's optional second value can be skipped. |
| `isVariadic` | It takes any number of values (`upload a b c`). |
| `optionsCanBreakVariadicArg` | `false`: an option after the values is one more value, not an option (default `true`). |
| `suggestions` | A fixed list of values (see [Suggestions](#suggestions)). |
| `template` | Paths: `"filepaths"` (files and folders) or `"folders"`. |
| `generators` | Values computed while you type (see [Generators](#generators)). |
| `isCommand` | A command line of its own starts here (`sudo`, `time`): its first word is a command, and the rest completes from that command's spec. |
| `commandNames` | It's a command's name (`which`, `man`): completed with the commands on your `PATH`. |

## Suggestions

In `suggestions` and `additionalSuggestions`, an entry is a string (`"staging"`), or a table
with the shared keys above and a `type`, for its icon: `file`, `folder`, `subcommand`,
`option`, `arg` or `shortcut` (Fig's `special` is a shortcut too).

## Generators

A generator gives an argument its values while you type. It's one of three kinds:

| Key | What it does |
| --- | --- |
| `builtin` | One of Lig's: `builtin = "git:branches"`. `lig specs generators` lists them. |
| `script` | A command whose output becomes the suggestions. A list (`["deploy", "list-services"]`) runs as is; a string (`"deploy list | sort"`) runs with `sh -c`. Both run in the shell's folder, with its environment. |
| `suggestions` | A fixed list, as an argument's `suggestions`. |
| `template` | Paths, as an argument's `template`. |

A script's output is read one line per suggestion, trimmed, empty lines skipped, unless:

| Key | What it does |
| --- | --- |
| `splitOn` | Split on this instead of newlines (`","`). |
| `skipLines` | Skip this many lines first, such as a header. |
| `field` | Keep one whitespace-separated column, counted from 0: `field = 0` is the first word. |
| `json` | The output is JSON (see below). |

With the script's output JSON, `json` has templates: text with JSON paths between `\u0001`
markers. Paths start at `$` and take `.key`, `[N]`, `[*]` (each item of a list), `{*}` (each
value of an object) and `{key}` (each key of an object); all the paths in a template walk the
same items.

```toml
# docker image ls --format json gives one JSON document per line.
generators = { script = ["docker", "image", "ls", "--format", "json"], json = { name = "\u0001$.Repository\u0001:\u0001$.Tag\u0001", description = "\u0001$.Size\u0001", lines = true } }
```

| Key in `json` | What it does |
| --- | --- |
| `name` | The suggestion (required). |
| `description` | Shown next to it. |
| `lines` | `true`: one JSON document per line, as many tools print. |

Any generator can also have:

| Key | What it does |
| --- | --- |
| `cache` | `{ ttl = 10000 }`: reuse the results for this many milliseconds (default `behavior.generator_cache_ms`). |
| `trigger` | Run again only when the typed text up to the last of these characters changes: `trigger = "/"` lists a folder once, not on every key. |
| `getQueryTerm` | Match only the text after the last of these characters: `getQueryTerm = ","` for `a,b,c` lists. |

## Parser directives

| Key in `parserDirectives` | What it does |
| --- | --- |
| `flagsArePosixNoncompliant` | `true`: `-abc` is one flag, not `-a -b -c` (as `find -name`, `java -jar`). |
| `optionArgSeparators` | What can join an option to its value, such as `["=", ":"]`. |

## Reusing specs

- **`loadSpec`** makes a command or subcommand complete as another spec: `ll.toml` with
  `name = "ll"` and `loadSpec = "ls"`. Keys next to it are added to that spec's.
- **`loadHelp`** reads a subcommand's options from its `--help` when it's entered, for the
  parts not worth writing out. Lig parses `--help` for commands without any spec on its own
  (`behavior.help_parsing`); `loadHelp` is for subcommands of a spec.
- An **alias** with a spec of its own completes from it: with `alias ls=eza`, `ls.toml` is
  used for `ls` and eza's spec for `eza`. Without one, an alias completes as what it runs.
- **`skipFigSpec`**, in Lig's own built-in specs, replaces the converted Fig spec of the same
  command instead of adding to it. Your specs always replace, so they don't need it.

## What Lig doesn't read

Fig specs are TypeScript; Lig reads their data, not their code. So a spec can't have
JavaScript generators (`postProcess`, `custom`, functions in `script`) or the `history` and
`help` templates. These Fig keys are accepted but change nothing: `default`, `isRequired`,
`dependsOn`, `isDangerous`, `deprecated`, `isScript`, `isModule` and `debounce`.

# lig: nu has no "line changed" hook, so lig reads the line off the screen, between
# the end of the prompt (nu's OSC 133;B mark) and the cursor. On a lig ssh or lig mosh host
# (mosh drops those marks) lig asks for the line with a key instead (see __lig_reply).
def __lig_clean [text: string] {
    $text | str replace --all "\e" "" | str replace --all (char bel) ""
}

# To the terminal, or under lig mosh to a file lig reads over ssh (mosh drops the messages).
def __lig_send [msg: string] {
    if ($env.__lig.out | is-empty) {
        print --no-newline $msg
    } else {
        $msg | save --append $env.__lig.out
    }
}

# Builtins and custom commands are sent once; aliases when they change. lig's state is a
# record in $env.__lig: nu passes only strings to the programs it runs, so they never see it.
def --env __lig_report [] {
    # nu runs pre_prompt after lig's key-bound commands too: that's no new prompt.
    if $env.__lig.host {
        $env.__lig.host = false
        return
    }
    mut out = ""
    # The line that just ran, and how it ended (for lig's history).
    if ($env.__lig.ran | is-not-empty) {
        $out += $"\e]6973;E;($env.LAST_EXIT_CODE);(__lig_clean $env.__lig.ran)(char bel)"
        $env.__lig.ran = ""
    }
    $out += $"\e]6973;C;(__lig_clean $env.PWD)(char bel)"
    $out += $"\e]6973;P;(__lig_clean ($env.PATH | str join (char esep)))(char bel)"
    let aliases = scope aliases | each {|a| $"($a.name)=($a.expansion)" } | str join (char record_separator) | __lig_clean $in
    if $aliases != $env.__lig.aliases {
        $env.__lig.aliases = $aliases
        $out += $"\e]6973;A;($aliases)(char bel)"
    }
    if not $env.__lig.reported {
        $env.__lig.reported = true
        let commands = scope commands
        let builtins = $commands | where type == "built-in" | get name | str join (char record_separator)
        let custom = $commands | where type == "custom" and not ($it.name | str starts-with "_") | get name | str join (char record_separator)
        $out += $"\e]6973;K;(__lig_clean $builtins)(char bel)\e]6973;N;(__lig_clean $custom)(char bel)"
    }
    # Ready, on a host: lig may ask for the line, and writes insertions to this file.
    if $env.__lig.remote {
        $out += $"\e]6973;R;($nu.pid);($env.LIG_INSERT_FILE);nu(char bel)"
    }
    __lig_send $out
}

# A line runs (lig's own key-bound commands, named __lig_…, aren't lines).
def --env __lig_ran [] {
    let line = commandline
    if ($line | str starts-with "__lig_") { return }
    $env.__lig.ran = $line
    if $env.__lig.remote { __lig_send $"\e]6973;F(char bel)" }
}

# On a host, Ctrl+Alt+Shift+F11: the line and the cursor (in code points, as lig counts),
# for lig's last request.
def --env __lig_reply [] {
    $env.__lig.host = true
    let line = commandline
    let left = $line | str substring --grapheme-clusters 0..<(commandline get-cursor)
    let cursor = $left | split chars --code-points | length
    __lig_send $"\e]6973;Q;0;B;($cursor);(__lig_clean $line)(char bel)"
}

# lig writes `<delete>;<back>;<text>` to $env.LIG_INSERT_FILE, then sends Ctrl+Alt+Shift+F12.
# On a host: `<delete>;<back>;<length>;<the line left of the cursor as lig saw it><text>`,
# checked first, since lig's view can be a round trip old.
def --env __lig_insert [] {
    $env.__lig.host = true
    let payload = open --raw $env.LIG_INSERT_FILE | into string
    let fields = if $env.__lig.remote { 4 } else { 3 }
    let parts = $payload | split row --number $fields ";"
    if ($parts | length) != $fields { return }
    let delete = $parts.0 | into int
    let back = $parts.1 | into int
    let line = commandline
    let cursor = commandline get-cursor
    let left = $line | str substring --grapheme-clusters 0..<$cursor
    let right = $line | str substring --grapheme-clusters $cursor..
    let text = if $env.__lig.remote {
        let chars = $parts.3 | split chars --code-points
        let seen = $chars | first ($parts.2 | into int) | str join
        if $seen != $left {
            __lig_send $"\e]6973;J(char bel)"
            return
        }
        $chars | skip ($parts.2 | into int) | str join
    } else {
        $parts.2
    }
    let kept = [(($left | str length --grapheme-clusters) - $delete) 0] | math max
    let left = $left | str substring --grapheme-clusters 0..<$kept
    commandline edit --replace $"($left)($text)($right)"
    commandline set-cursor (($left | str length --grapheme-clusters) + ($text | str length --grapheme-clusters) - $back)
}

if $nu.is-interactive and ($env.LIG_DISABLE? | is-empty) and ($env.LIG_TTY? | is-not-empty) and ($env.LIG_TTY == (^tty | complete | get stdout | str trim)) {
    let remote = $env.LIG_REMOTE? | is-not-empty
    $env.__lig = {ran: "", aliases: "", reported: false, host: false, remote: $remote, out: ($env.LIG_OUT? | default "")}
    hide-env --ignore-errors LIG_OUT
    # On a host lig can't write our files: it writes insertions over its ssh connection here.
    if $remote { $env.LIG_INSERT_FILE = (^mktemp | str trim) }
    $env.config.shell_integration.osc133 = true
    $env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {|| __lig_report })
    $env.config.hooks.pre_execution = ($env.config.hooks.pre_execution? | default [] | append {|| __lig_ran })
    mut keys = [{
        name: lig_insert
        modifier: control_alt_shift
        keycode: f12
        mode: [emacs vi_insert vi_normal]
        event: { send: executehostcommand cmd: "__lig_insert" }
    }]
    if $remote {
        $keys ++= [{
            name: lig_reply
            modifier: control_alt_shift
            keycode: f11
            mode: [emacs vi_insert vi_normal]
            event: { send: executehostcommand cmd: "__lig_reply" }
        }]
    }
    $env.config.keybindings = ($env.config.keybindings? | default [] | append $keys)
}

# lig: nu has no "line changed" hook, so lig reads the line off the screen, between
# the end of the prompt (nu's OSC 133;B mark) and the cursor.
def __lig_clean [text: string] {
    $text | str replace --all "\e" "" | str replace --all (char bel) ""
}

# Builtins and custom commands are sent once; aliases when they change. lig's state is a
# record in $env.__lig: nu passes only strings to the programs it runs, so they never see it.
def --env __lig_report [] {
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
    print --no-newline $out
}

# lig writes `<delete>;<back>;<text>` to $env.LIG_INSERT_FILE, then sends Ctrl+Alt+Shift+F12.
def __lig_insert [] {
    let parts = open --raw $env.LIG_INSERT_FILE | into string | split row --number 3 ";"
    if ($parts | length) != 3 { return }
    let delete = $parts.0 | into int
    let back = $parts.1 | into int
    let text = $parts.2
    let line = commandline
    let cursor = commandline get-cursor
    let left = $line | str substring --grapheme-clusters 0..<$cursor
    let right = $line | str substring --grapheme-clusters $cursor..
    let kept = [(($left | str length --grapheme-clusters) - $delete) 0] | math max
    let left = $left | str substring --grapheme-clusters 0..<$kept
    commandline edit --replace $"($left)($text)($right)"
    commandline set-cursor (($left | str length --grapheme-clusters) + ($text | str length --grapheme-clusters) - $back)
}

if $nu.is-interactive and ($env.LIG_DISABLE? | is-empty) and ($env.LIG_TTY? | is-not-empty) and ($env.LIG_TTY == (^tty | complete | get stdout | str trim)) {
    $env.__lig = {ran: "", aliases: "", reported: false}
    $env.config.shell_integration.osc133 = true
    $env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {|| __lig_report })
    $env.config.hooks.pre_execution = ($env.config.hooks.pre_execution? | default [] | append {|| $env.__lig.ran = (commandline) })
    $env.config.keybindings = ($env.config.keybindings? | default [] | append {
        name: lig_insert
        modifier: control_alt_shift
        keycode: f12
        mode: [emacs vi_insert vi_normal]
        event: { send: executehostcommand cmd: "__lig_insert" }
    })
}

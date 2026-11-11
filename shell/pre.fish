# Wrap fish in lig unless lig itself started it; `fish` typed in another shell
# inside lig gets its own lig, like a tmux pane.
if status is-interactive; and test -z "$LIG_DISABLE"; and isatty stdin; and isatty stdout; and test "$TERM" != dumb; and test -z "$INSIDE_EMACS"; and test -x @LIG_BIN@
    # Parent pid: /proc on Linux (ps may be missing there), ps on macOS.
    set -l __lig_parent
    if test -r /proc/$fish_pid/stat
        set __lig_parent (string split ' ' -- (string replace -r '^.*\) ' '' < /proc/$fish_pid/stat))[2]
    else if command -q ps
        set __lig_parent (string trim -- (command ps -o ppid= -p $fish_pid))
    end
    # An unknown parent counts as lig: wrapping again would start lig in a loop.
    if test "$LIG_TTY" != (tty); or begin; test -n "$__lig_parent"; and test "$__lig_parent" != "$LIG_PID"; end
        if status is-login
            exec @LIG_BIN@ shell --shell (status fish-path) --login
        else
            exec @LIG_BIN@ shell --shell (status fish-path)
        end
    end
end

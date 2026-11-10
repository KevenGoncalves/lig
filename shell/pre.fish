# Wrap fish in lig unless lig itself started it; `fish` typed in another shell
# inside lig gets its own lig, like a tmux pane.
if status is-interactive; and test -z "$LIG_DISABLE"; and isatty stdin; and isatty stdout; and test "$TERM" != dumb; and test -z "$INSIDE_EMACS"; and test -x @LIG_BIN@
    # ps is available on both Linux and macOS.
    set -l __lig_parent (string trim -- (command ps -o ppid= -p $fish_pid))
    if test "$LIG_TTY" != (tty); or test "$__lig_parent" != "$LIG_PID"
        if status is-login
            exec @LIG_BIN@ shell --shell (status fish-path) --login
        else
            exec @LIG_BIN@ shell --shell (status fish-path)
        end
    end
end

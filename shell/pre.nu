# lig: run this interactive nu inside lig (once per terminal; Linux and macOS). Not Midnight
# Commander's subshell (MC_SID).
if $nu.os-info.family == "unix" and $nu.is-interactive and ($env.LIG_DISABLE? | is-empty) and (is-terminal --stdin) and (is-terminal --stdout) and ($env.TERM? != "dumb") and ($env.INSIDE_EMACS? | is-empty) and ($env.MC_SID? | is-empty) and (r#'@LIG_BIN@'# | path exists) and ($env.LIG_TTY? != (^tty | complete | get stdout | str trim)) {
    if $nu.is-login {
        exec r#'@LIG_BIN@'# shell --shell $nu.current-exe --login
    } else {
        exec r#'@LIG_BIN@'# shell --shell $nu.current-exe
    }
}

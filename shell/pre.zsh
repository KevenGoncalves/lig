# lig: wrap this interactive shell in the lig PTY (once per terminal).
if [[ -o interactive && -z "$LIG_DISABLE" && "$LIG_TTY" != "$TTY" && -t 0 && -t 1 \
      && "$TERM" != dumb && -z "$INSIDE_EMACS" && -x "@LIG_BIN@" ]]; then
  __lig_shell_path=$("@LIG_BIN@" shell-path $$) || return
  if [[ -o login ]]; then
    builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path" --login
  else
    builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path"
  fi
fi

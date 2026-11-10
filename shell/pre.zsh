# lig: wrap this interactive shell in the lig PTY (once per terminal).
if [[ -o interactive && -z "$LIG_DISABLE" && "$LIG_TTY" != "$TTY" && -t 0 && -t 1 \
      && "$TERM" != dumb && -z "$INSIDE_EMACS" && -x "@LIG_BIN@" ]]; then
  # On failure, start without lig: `return` here would skip the rest of .zshrc.
  if __lig_shell_path=$("@LIG_BIN@" shell-path $$); then
    if [[ -o login ]]; then
      builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path" --login
    else
      builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path"
    fi
  fi
  unset __lig_shell_path
fi

# lig: wrap this interactive shell in the lig PTY (once per terminal). Not Midnight Commander's
# subshell (MC_SID): it has a terminal of its own, but mc drives it.
if [[ -o interactive && -z "$LIG_DISABLE" && "$LIG_TTY" != "$TTY" && -t 0 && -t 1 \
      && "$TERM" != dumb && -z "$INSIDE_EMACS" && -z "$MC_SID" && -x "@LIG_BIN@" ]]; then
  # On failure, start without lig: `return` here would skip the rest of .zshrc.
  if __lig_shell_path=$("@LIG_BIN@" shell-path $$); then
    # Ghostty's integration loaded here only; post.zsh loads it in the shell inside lig.
    (( ${+_ghostty_state} )) && builtin export LIG_GHOSTTY=1
    if [[ -o login ]]; then
      builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path" --login
    else
      builtin exec "@LIG_BIN@" shell --shell "$__lig_shell_path"
    fi
  fi
  unset __lig_shell_path
fi

# lig: wrap this interactive bash in the lig PTY (once per terminal). Not Midnight Commander's
# subshell (MC_SID): it has a terminal of its own, but mc drives it.
if [[ $- == *i* && -z "$LIG_DISABLE" && -t 0 && -t 1 && "$TERM" != dumb && -z "$INSIDE_EMACS" \
      && -z "$MC_SID" && -x "@LIG_BIN@" && "$LIG_TTY" != "$(tty)" ]]; then
  # Ghostty's integration sources this file; post.bash loads it in the shell inside lig.
  [[ " ${BASH_SOURCE[*]}" == *"/shell-integration/bash/ghostty.bash"* ]] && export LIG_GHOSTTY=1
  if shopt -q login_shell; then
    exec "@LIG_BIN@" shell --shell "$BASH" --login
  else
    exec "@LIG_BIN@" shell --shell "$BASH"
  fi
fi

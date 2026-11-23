# lig ssh: start the login shell with lig's hooks, from a temp dir it removes at its first prompt.
s=${SHELL:-/bin/sh}
# A terminal the host has no terminfo for (xterm-ghostty, xterm-kitty) breaks clear, less, vim…
{ infocmp "$TERM" || tput -T "$TERM" longname; } >/dev/null 2>&1 || export TERM=xterm-256color
# The terminal by its own name (/dev/pts/3), from stderr: stdin is this script. A shell reading
# from /dev/tty would pass that on, and tmux won't start there ("can't use /dev/tty").
LIG_TTY=$(tty <&2) || LIG_TTY=$(tty </dev/tty) || exec "$s" -l </dev/tty
d=$(mktemp -d "${TMPDIR:-/tmp}/lig.XXXXXX") || exec "$s" -l <"$LIG_TTY"
export LIG_TTY LIG_REMOTE=1 LIG_DIR="$d"
# lig mosh: the hooks' messages go to this file, which lig reads over ssh.
if [ -n "${LIG_OUT-}" ]; then
  (umask 077 && : >>"$LIG_OUT") && export LIG_OUT || unset LIG_OUT
fi
case ${s##*/} in
zsh)
  cat >"$d/.zshenv" <<'LIG_EOF'
@ZSHENV@
LIG_EOF
  cat >"$d/post.zsh" <<'LIG_EOF'
@POST_ZSH@
LIG_EOF
  [ -n "${ZDOTDIR+x}" ] && export LIG_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR=$d exec "$s" -l <"$LIG_TTY"
  ;;
bash)
  cat >"$d/rc" <<'LIG_EOF'
@BASHRC@
@POST_BASH@
_lig_cleanup
LIG_EOF
  exec "$s" --rcfile "$d/rc" -i <"$LIG_TTY"
  ;;
fish)
  cat >"$d/post.fish" <<'LIG_EOF'
@POST_FISH@
LIG_EOF
  # After the user's config, as an install puts it at its end.
  exec "$s" -l -C "source '$d/post.fish'; command rm -rf '$d'" <"$LIG_TTY"
  ;;
nu)
  cat >"$d/post.nu" <<'LIG_EOF'
@POST_NU@
LIG_EOF
  # After the user's config, as an install puts it at its end.
  exec "$s" --login --execute "source '$d/post.nu'; rm --recursive --permanent '$d'" <"$LIG_TTY"
  ;;
esac
rm -rf "$d"
t=$LIG_TTY
unset LIG_TTY LIG_REMOTE LIG_DIR
exec "$s" -l <"$t"

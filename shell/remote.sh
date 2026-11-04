# lig ssh: start the login shell with lig's hooks, from a temp dir it removes at its first prompt.
s=${SHELL:-/bin/sh}
# A terminal the host has no terminfo for (xterm-ghostty, xterm-kitty) breaks clear, less, vim…
{ infocmp "$TERM" || tput -T "$TERM" longname; } >/dev/null 2>&1 || export TERM=xterm-256color
LIG_TTY=$(tty </dev/tty) || exec "$s" -l </dev/tty
d=$(mktemp -d "${TMPDIR:-/tmp}/lig.XXXXXX") || exec "$s" -l </dev/tty
export LIG_TTY LIG_REMOTE=1 LIG_DIR="$d"
case ${s##*/} in
zsh)
  cat >"$d/.zshenv" <<'LIG_EOF'
@ZSHENV@
LIG_EOF
  cat >"$d/post.zsh" <<'LIG_EOF'
@POST_ZSH@
LIG_EOF
  [ -n "${ZDOTDIR+x}" ] && export LIG_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR=$d exec "$s" -l </dev/tty
  ;;
bash)
  cat >"$d/rc" <<'LIG_EOF'
@BASHRC@
@POST_BASH@
_lig_cleanup
LIG_EOF
  exec "$s" --rcfile "$d/rc" -i </dev/tty
  ;;
esac
rm -rf "$d"
unset LIG_TTY LIG_REMOTE LIG_DIR
exec "$s" -l </dev/tty

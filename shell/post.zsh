# lig: report the ZLE edit buffer to the lig PTY and accept insertions from it.
if [[ -o interactive && -z "$LIG_DISABLE" && -n "$LIG_TTY" && "$LIG_TTY" == "$TTY" ]] && (( ! ${+_lig_loaded} )); then
  typeset -g _lig_loaded=1 _lig_last=

  _lig_report() {
    builtin emulate -L zsh
    local msg
    # No popup during history browsing or zsh's own completion: those keys stay with zsh.
    if (( ${ISEARCHMATCH_ACTIVE:-0} )) || [[ $KEYMAP == (vicmd|menuselect) ]] || [[ -n $PREBUFFER ]] \
        || [[ $LASTWIDGET == (*history*|*beginning-search*|up-line*|down-line*|*-search-*|*complete*|*expand*|menu-*) ]]; then
      msg=$'\e]6973;H\a'
    else
      # Mask control chars so they can't end the OSC; replacing keeps $CURSOR valid.
      msg=$'\e]6973;B;'"${CURSOR};${BUFFER//[$'\a\e']/ }"$'\a'
    fi
    [[ $msg == $_lig_last ]] && return
    _lig_last=$msg
    builtin print -rn -- "$msg" >$TTY
  }
  _lig_line_init() { _lig_last=; _lig_report }
  _lig_line_finish() {
    _lig_last=
    builtin print -rn -- $'\e]6973;F\a' >$TTY
  }

  # lig sends: ESC [9147~ <delete>;<back>;<text> BEL
  _lig_insert() {
    builtin emulate -L zsh
    local c payload=
    while builtin read -k 1 c; do
      [[ $c == $'\a' ]] && break
      payload+=$c
    done
    local del=${payload%%;*}
    payload=${payload#*;}
    local back=${payload%%;*}
    local text=${payload#*;}
    (( del > 0 )) && LBUFFER=${LBUFFER[1,-del-1]}
    LBUFFER+=$text
    (( back > 0 )) && (( CURSOR -= back ))
    _lig_last=
    # zsh-autosuggestions: drop the stale suggestion and fetch a fresh one.
    unset POSTDISPLAY
    (( ${+functions[_zsh_autosuggest_fetch]} )) && _zsh_autosuggest_fetch
  }
  # zsh-autosuggestions' widget wrapper would restore the old suggestion mid-read.
  typeset -ga ZSH_AUTOSUGGEST_IGNORE_WIDGETS
  ZSH_AUTOSUGGEST_IGNORE_WIDGETS+=(_lig_insert)

  # Generators need the shell's cwd and PATH; aliases/functions sent on change (split by \x1e).
  typeset -g _lig_aliases= _lig_functions= _lig_builtins_sent=
  _lig_precmd() {
    builtin emulate -L zsh
    builtin print -rn -- $'\e]6973;C;'"${PWD//[$'\a\e']/}"$'\a'$'\e]6973;P;'"${PATH//[$'\a\e']/}"$'\a' >$TTY
    local k
    local -a pairs
    for k in ${(k)aliases}; do pairs+=("$k=${aliases[$k]}"); done
    local a=${(pj:\x1e:)pairs}
    a=${a//[$'\a\e']/}
    if [[ $a != $_lig_aliases ]]; then
      _lig_aliases=$a
      builtin print -rn -- $'\e]6973;A;'"$a"$'\a' >$TTY
    fi
    if [[ -z $_lig_builtins_sent ]]; then
      _lig_builtins_sent=1
      builtin print -rn -- $'\e]6973;K;'"${(pj:\x1e:)${(@k)builtins:#[^a-z]*}}"$'\a' >$TTY
    fi
    local f=${(pj:\x1e:)${(k)functions:#[_+.-]*}}
    if [[ $f != $_lig_functions ]]; then
      _lig_functions=$f
      builtin print -rn -- $'\e]6973;N;'"${f//[$'\a\e']/}"$'\a' >$TTY
    fi
  }
  builtin autoload -Uz add-zsh-hook
  add-zsh-hook precmd _lig_precmd

  builtin autoload -Uz add-zle-hook-widget
  add-zle-hook-widget line-init _lig_line_init
  add-zle-hook-widget line-pre-redraw _lig_report
  add-zle-hook-widget line-finish _lig_line_finish
  builtin zle -N _lig_insert
  builtin bindkey -M emacs '\e[9147~' _lig_insert
  builtin bindkey -M viins '\e[9147~' _lig_insert
  builtin bindkey -M vicmd '\e[9147~' _lig_insert
fi

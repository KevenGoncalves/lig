# lig: report the ZLE edit buffer to the lig PTY and accept insertions from it.
if [[ -o interactive && -z "$LIG_DISABLE" && -n "$LIG_TTY" && "$LIG_TTY" == "$TTY" ]] && (( ! ${+_lig_loaded} )); then
  typeset -g _lig_loaded=1 _lig_last=
  # Where messages go: the terminal, or under lig mosh a file lig reads over ssh (mosh drops them).
  typeset -g _lig_out=${LIG_OUT:-$TTY}
  unset LIG_OUT
  # Only this shell speaks to lig: a child that inherits its hooks has another pid, as the
  # zpty one zsh-autosuggestions' completion strategy runs ($$ stays the parent's there).
  builtin zmodload zsh/system 2>/dev/null
  typeset -g _lig_pid=${sysparams[pid]}

  _lig_report() {
    [[ ${sysparams[pid]} == $_lig_pid ]] || return 0
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
    builtin print -rn -- "$msg" >>$_lig_out
  }
  _lig_line_init() { _lig_last=; _lig_report }
  _lig_line_finish() {
    [[ ${sysparams[pid]} == $_lig_pid ]] || return 0
    _lig_last=
    _lig_ran=$BUFFER
    builtin print -rn -- $'\e]6973;F\a' >>$_lig_out
  }

  # lig sends: ESC [9147~ <delete>;<back>;<text> US <the line left of the cursor, as lig saw it> BEL
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
    local seen=${text#*$'\x1f'}
    text=${text%%$'\x1f'*}
    # The line changed since lig saw it (over lig ssh, a report can come late): say so, change nothing.
    if [[ $LBUFFER != "$seen" ]]; then
      builtin print -rn -- $'\e]6973;J\a' >>$_lig_out
      return
    fi
    (( del > 0 )) && LBUFFER=${LBUFFER[1,-del-1]}
    LBUFFER+=$text
    (( back > 0 )) && (( CURSOR -= back ))
    _lig_last=
    # zsh-autosuggestions: drop the stale suggestion and fetch a fresh one.
    unset POSTDISPLAY
    # `if`, not `&&`: a widget that returns non-zero beeps.
    if (( ${+functions[_zsh_autosuggest_fetch]} )); then _zsh_autosuggest_fetch; fi
  }
  # zsh-autosuggestions' widget wrapper would restore the old suggestion mid-read.
  typeset -ga ZSH_AUTOSUGGEST_IGNORE_WIDGETS
  ZSH_AUTOSUGGEST_IGNORE_WIDGETS+=(_lig_insert)

  # Generators need the shell's cwd and PATH; aliases, functions and exported variables sent on change (split by \x1e).
  typeset -g _lig_aliases= _lig_functions= _lig_vars= _lig_builtins_sent= _lig_ran= _lig_histfile=
  _lig_precmd() {
    local ret=$?
    [[ ${sysparams[pid]} == $_lig_pid ]] || return ret
    builtin emulate -L zsh
    # The line that just ran, and how it ended (for lig's history).
    if [[ -n $_lig_ran ]]; then
      builtin print -rn -- $'\e]6973;E;'"$ret;${_lig_ran//[$'\a\e']/ }"$'\a' >>$_lig_out
      _lig_ran=
    fi
    builtin print -rn -- $'\e]6973;C;'"${PWD//[$'\a\e']/}"$'\a'$'\e]6973;P;'"${PATH//[$'\a\e']/}"$'\a' >>$_lig_out
    # Where zsh keeps its history, for lig's to start from: HISTFILE isn't exported.
    if [[ -n $HISTFILE && ${HISTFILE:a} != $_lig_histfile ]]; then
      _lig_histfile=${HISTFILE:a}
      builtin print -rn -- $'\e]6973;L;zsh;'"${_lig_histfile//[$'\a\e']/}"$'\a' >>$_lig_out
    fi
    local k
    local -a pairs
    for k in ${(k)aliases}; do pairs+=("$k=${aliases[$k]}"); done
    local a=${(pj:\x1e:)pairs}
    a=${a//[$'\a\e']/}
    if [[ $a != $_lig_aliases ]]; then
      _lig_aliases=$a
      builtin print -rn -- $'\e]6973;A;'"$a"$'\a' >>$_lig_out
    fi
    if [[ -z $_lig_builtins_sent ]]; then
      _lig_builtins_sent=1
      builtin print -rn -- $'\e]6973;K;'"${(pj:\x1e:)${(@k)builtins:#[^a-z]*}}"$'\a' >>$_lig_out
    fi
    local f=${(pj:\x1e:)${(k)functions:#[_+.-]*}}
    if [[ $f != $_lig_functions ]]; then
      _lig_functions=$f
      builtin print -rn -- $'\e]6973;N;'"${f//[$'\a\e']/}"$'\a' >>$_lig_out
    fi
    local -a vars
    for k in ${(k)parameters[(R)*export*]}; do [[ $k == _ ]] || vars+=("$k=${(P)k}"); done
    local v=${(pj:\x1e:)vars}
    v=${v//[$'\a\e']/}
    if [[ $v != $_lig_vars ]]; then
      _lig_vars=$v
      builtin print -rn -- $'\e]6973;V;'"$v"$'\a' >>$_lig_out
    fi
    # Prompts after this hook (starship…) still see the command's status.
    return ret
  }
  # First, to see the command's own $? (other hooks' commands would change it).
  precmd_functions=(_lig_precmd ${precmd_functions:#_lig_precmd})

  builtin autoload -Uz add-zle-hook-widget
  builtin zmodload zsh/zleparameter 2>/dev/null
  # Run widget $2 on ZLE hook $1. When another widget already wraps the hook's dispatcher
  # (fast-syntax-highlighting does, after history-substring-search hooked zle-line-finish),
  # add-zle-hook-widget would make that widget call itself without end: chain onto it instead.
  _lig_hook() {
    local hook=zle-$1
    if [[ -z ${widgets[$hook]-} || ${widgets[$hook]} == user:azhw:$hook ]]; then
      add-zle-hook-widget $1 $2
      return
    fi
    builtin zle -A $hook _lig_prev_$hook
    builtin eval "_lig_chain_$hook() { $2; builtin zle _lig_prev_$hook -- \"\$@\" }"
    builtin zle -N $hook _lig_chain_$hook
  }
  _lig_hook line-init _lig_line_init
  _lig_hook line-pre-redraw _lig_report
  _lig_hook line-finish _lig_line_finish
  builtin zle -N _lig_insert
  builtin bindkey -M emacs '\e[9147~' _lig_insert
  builtin bindkey -M viins '\e[9147~' _lig_insert
  builtin bindkey -M vicmd '\e[9147~' _lig_insert

  # lig asks with ESC [9148~ what Tab would offer (behavior.completers = ["shell"]): the
  # matches are collected as zsh adds them, then sent; the line and the screen stay as they are.
  typeset -ga _lig_matches
  _lig_compadd() {
    # Queries (-O, -A, -D) are zsh's own business.
    if (( ${@[(I)-[OAD]*]} )); then builtin compadd "$@"; return; fi
    local -a hits shown
    local display= i
    for (( i = 1; i <= $#; i++ )); do
      [[ ${@[i]} == -d ]] && display=${@[i+1]}
      [[ ${@[i]} == -d?* ]] && display=${@[i]#-d}
    done
    builtin compadd -A hits "$@"
    [[ -n $display ]] && shown=( "${(@P)display}" )
    for (( i = 1; i <= $#hits; i++ )); do _lig_matches+=( "${hits[i]}"$'\t'"${shown[i]}" ); done
    builtin compadd "$@"
  }
  _lig_quiet() { compstate[insert]=''; compstate[list]=''; }
  _lig_complete() {
    [[ ${sysparams[pid]} == $_lig_pid ]] || return 0
    _lig_matches=()
    if (( $+functions[_main_complete] )); then
      local -a comppostfuncs=( _lig_quiet )
      compadd() { _lig_compadd "$@" }
      _main_complete
      builtin unfunction compadd
    fi
    local m=${(pj:\x1e:)_lig_matches}
    builtin print -rn -- $'\e]6973;Z;'"${LBUFFER//[$'\a\e\x1d']/ }"$'\x1d'"${m//[$'\a\e']/ }"$'\a' >>$_lig_out
  }
  # Not named *complete*: _lig_report would take the next prompt for zsh's own menu.
  builtin zle -C _lig_ask complete-word _lig_complete
  builtin bindkey -M emacs '\e[9148~' _lig_ask
  builtin bindkey -M viins '\e[9148~' _lig_ask
  builtin bindkey -M vicmd '\e[9148~' _lig_ask

  # Ghostty's prompt marks (OSC 133) tell it the shell is idle, so closing it needs no confirmation.
  if [[ -n $LIG_GHOSTTY ]]; then
    builtin unset LIG_GHOSTTY
    if (( ! ${+_ghostty_state} )) && [[ -r $GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration ]]; then
      builtin source "$GHOSTTY_RESOURCES_DIR/shell-integration/zsh/ghostty-integration"
    fi
  fi
fi

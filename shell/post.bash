# lig: bash has no "line changed" hook, so lig reads the line off the screen
# between the prompt marker and the cursor.
if [[ $- == *i* && -z "$LIG_DISABLE" && -n "$LIG_TTY" && "$LIG_TTY" == "$(tty)" && -z "$__lig_loaded" ]]; then
  __lig_loaded=1
  # Where messages go: the terminal, or under lig mosh a file lig reads over ssh (mosh drops them).
  __lig_out=${LIG_OUT:-/dev/tty}
  # Over lig mosh lig can't read the line off the screen (mosh drops the prompt's marker):
  # bash says when it's at a prompt, and lig asks for the line (see __lig_query).
  __lig_asked=${LIG_OUT:+1}
  unset LIG_OUT
  __lig_aliases=
  __lig_vars=
  __lig_once=
  __lig_hist=
  __lig_histcmd=

  # First in PROMPT_COMMAND, before other hooks change $?: how the line ended.
  __lig_status() {
    local s=$?
    __lig_ran_status=$s __lig_ran_histcmd=$HISTCMD
    return $s
  }

  __lig_prompt() {
    local status=$?
    # The line that just ran, and how it ended (for lig's history): a new history entry.
    # HISTCMD moves on when a line ran, so an empty Enter forks nothing.
    if [[ $__lig_ran_histcmd != "$__lig_histcmd" ]]; then
      __lig_histcmd=$__lig_ran_histcmd
      local h
      h=$(HISTTIMEFORMAT= builtin history 1)
      # `history 1` prints the number, `*` if edited, a space, then the line as typed.
      if [[ $h =~ ^\ *([0-9]+)[*\ ]\ (.*)$ ]]; then
        [[ -n $__lig_hist && ${BASH_REMATCH[1]} != "$__lig_hist" ]] &&
          printf '\e]6973;E;%s;%s\a' "$__lig_ran_status" "${BASH_REMATCH[2]//[$'\a\e']/ }" >>"$__lig_out"
        __lig_hist=${BASH_REMATCH[1]}
      else
        # No history yet: the next line is entry 1.
        __lig_hist=${__lig_hist:-0}
      fi
    fi
    printf '\e]6973;C;%s\a\e]6973;P;%s\a' "${PWD//[$'\a\e']/}" "${PATH//[$'\a\e']/}" >>"$__lig_out"
    # Where bash keeps its history, for lig's to start from: HISTFILE isn't exported.
    if [[ $HISTFILE == /* && $HISTFILE != "$__lig_histfile" ]]; then
      __lig_histfile=$HISTFILE
      printf '\e]6973;L;bash;%s\a' "${HISTFILE//[$'\a\e']/}" >>"$__lig_out"
    fi
    local k a=
    for k in "${!BASH_ALIASES[@]}"; do a+="$k=${BASH_ALIASES[$k]}"$'\x1e'; done
    a=${a%$'\x1e'}
    a=${a//[$'\a\e']/}
    if [[ $a != "$__lig_aliases" ]]; then
      __lig_aliases=$a
      printf '\e]6973;A;%s\a' "$a" >>"$__lig_out"
    fi
    local e=
    for k in $(compgen -e); do [[ $k == _ ]] || e+="$k=${!k}"$'\x1e'; done
    e=${e%$'\x1e'}
    e=${e//[$'\a\e']/}
    if [[ $e != "$__lig_vars" ]]; then
      __lig_vars=$e
      printf '\e]6973;V;%s\a' "$e" >>"$__lig_out"
    fi
    if [[ -z $__lig_once ]]; then
      __lig_once=1
      local -a b f
      local x
      while IFS= read -r x; do b+=("$x"); done < <(compgen -b)
      while IFS= read -r x; do f+=("$x"); done < <(compgen -A function -X '_*')
      local IFS=$'\x1e'
      printf '\e]6973;K;%s\a\e]6973;N;%s\a' "${b[*]}" "${f[*]}" >>"$__lig_out"
    fi
    if [[ -n $__lig_asked ]]; then
      printf '\e]6973;R;%s\a' "$$" >>"$__lig_out"
    # Re-append the marker when another tool (starship, …) rebuilds PS1.
    elif [[ $PS1 != *'6973;S'* ]]; then
      # A prompt ending in an odd backslash would swallow ours: complete it.
      local t=${PS1##*[!\\]}
      (( ${#t} % 2 )) && PS1+='\'
      PS1+='\[\e]6973;S\a\]'
    fi
    return $status
  }
  # __lig_status first, __lig_prompt last (after tools that rebuild PS1).
  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(__lig_status "${PROMPT_COMMAND[@]}" __lig_prompt)
  else
    PROMPT_COMMAND="__lig_status"$'\n'"${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}__lig_prompt"
  fi
  if [[ -n $__lig_asked ]]; then
    # The line runs: said with the messages, where mosh can't drop it.
    __lig_run() { printf '\e]6973;F\a' >>"$__lig_out"; }
    [[ $PS0 == *__lig_run* ]] || PS0='$(__lig_run)'"${PS0:-}"
  else
    # Prepended: vte.sh (GNOME Terminal) ends PS0 with `ESC \`, which would swallow our `\e`.
    [[ $PS0 == *'6973;F'* ]] || PS0='\e]6973;F\a'"${PS0:-}"
  fi

  # lig writes `<delete>;<back>;<text>` to $LIG_INSERT_FILE, then sends ESC [9147~;
  # over `lig ssh` the payload follows the key instead, ending in BEL.
  __lig_insert() {
    local p
    if [[ -n $LIG_REMOTE ]]; then
      IFS= read -r -s -d $'\a' p || return
    else
      # Bash 5.2's $(<file) shortcut inside bind -x can discard the completed
      # operands when the line runs. Read directly, preserving trailing newlines.
      IFS= read -r -d '' p < "$LIG_INSERT_FILE" || [[ -n $p ]] || return
    fi
    local del=${p%%;*}
    p=${p#*;}
    local back=${p%%;*}
    local text=${p#*;}
    local left=${READLINE_LINE:0:READLINE_POINT} right=${READLINE_LINE:READLINE_POINT}
    # Asked for its line, bash gets it checked: <length>;<the line left of the cursor><text>.
    if [[ -n $__lig_asked ]]; then
      local n=${text%%;*}
      text=${text#*;}
      if [[ ${text:0:n} != "$left" ]]; then
        printf '\e]6973;J\a' >>"$__lig_out"
        return
      fi
      text=${text:n}
    fi
    if (( del >= ${#left} )); then
      left=
    elif (( del > 0 )); then
      left=${left:0:${#left}-del}
    fi
    READLINE_LINE=$left$text$right
    READLINE_POINT=$(( ${#left} + ${#text} - back ))
  }
  bind -x '"\e[9147~": __lig_insert'

  # lig mosh: lig asks for the line with ESC [9149~ <id> BEL; the answer goes with the messages.
  __lig_query() {
    local id
    IFS= read -r -s -d $'\a' id || return
    printf '\e]6973;Q;%s;B;%s;%s\a' "$id" "$READLINE_POINT" "${READLINE_LINE//[$'\a\e']/ }" >>"$__lig_out"
  }
  [[ -n $__lig_asked ]] && bind -x '"\e[9149~": __lig_query'

  # lig asks with ESC [9148~ what Tab would offer (behavior.completers = ["shell"]): the
  # command's completion (bash-completion's function, `complete -W` words) runs as Tab
  # would run it, and its matches are sent back; the line stays as it is. No command
  # substitution: in bash 5.2 one inside `bind -x` can lose the line (see __lig_insert).
  __lig_complete() {
    local line=${READLINE_LINE:0:READLINE_POINT} spec= words= func= f="$LIG_QUERY_FILE.bash"
    local -a COMP_WORDS COMPREPLY=()
    read -ra COMP_WORDS <<< "$line"
    [[ -z $line || $line == *[[:space:]] ]] && COMP_WORDS+=("")
    local COMP_CWORD=$(( ${#COMP_WORDS[@]} - 1 )) COMP_LINE=$line COMP_POINT=${#line} COMP_TYPE=9 COMP_KEY=9
    local cmd=${COMP_WORDS[0]} cur=${COMP_WORDS[COMP_CWORD]} prev=${COMP_WORDS[COMP_CWORD-1]}
    if (( COMP_CWORD > 0 )); then
      complete -p -- "$cmd" > "$f" 2>/dev/null || {
        # Not loaded yet: bash-completion's default (`complete -D`) loads it.
        complete -p -D > "$f" 2>/dev/null && IFS= read -r spec < "$f" &&
          [[ $spec =~ -F\ ([^ ]+) ]] && "${BASH_REMATCH[1]}" "$cmd" "$cur" "$prev" >/dev/null 2>&1
        complete -p -- "$cmd" > "$f" 2>/dev/null
      }
      IFS= read -r spec < "$f"
      [[ $spec =~ -F\ ([^ ]+) ]] && func=${BASH_REMATCH[1]}
      [[ $spec =~ -W\ \'([^\']*)\' ]] && words=${BASH_REMATCH[1]}
      if [[ -n $func ]]; then
        "$func" "$cmd" "$cur" "$prev" >/dev/null 2>&1
      elif [[ -n $words ]]; then
        compgen -W "$words" -- "$cur" > "$f" 2>/dev/null && mapfile -t COMPREPLY < "$f"
      fi
    fi
    local IFS=$'\x1e'
    local m="${COMPREPLY[*]}"
    printf '\e]6973;Z;%s\x1d%s\a' "${line//[$'\a\e\x1d']/ }" "${m//[$'\a\e']/ }" >>"$__lig_out"
  }
  bind -x '"\e[9148~": __lig_complete'

  # Ghostty's prompt marks (OSC 133) tell it the shell is idle, so closing it needs no confirmation.
  if [[ -n $LIG_GHOSTTY ]]; then
    unset LIG_GHOSTTY
    if ! declare -F __ghostty_precmd >/dev/null && [[ -r $GHOSTTY_RESOURCES_DIR/shell-integration/bash/ghostty.bash ]]; then
      builtin source "$GHOSTTY_RESOURCES_DIR/shell-integration/bash/ghostty.bash"
    fi
  fi
fi

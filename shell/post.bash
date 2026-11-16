# lig: bash has no "line changed" hook, so lig reads the line off the screen
# between the prompt marker and the cursor.
if [[ $- == *i* && -z "$LIG_DISABLE" && -n "$LIG_TTY" && "$LIG_TTY" == "$(tty)" && -z "$__lig_loaded" ]]; then
  __lig_loaded=1
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
          printf '\e]6973;E;%s;%s\a' "$__lig_ran_status" "${BASH_REMATCH[2]//[$'\a\e']/ }" >/dev/tty
        __lig_hist=${BASH_REMATCH[1]}
      else
        # No history yet: the next line is entry 1.
        __lig_hist=${__lig_hist:-0}
      fi
    fi
    printf '\e]6973;C;%s\a\e]6973;P;%s\a' "${PWD//[$'\a\e']/}" "${PATH//[$'\a\e']/}" >/dev/tty
    local k a=
    for k in "${!BASH_ALIASES[@]}"; do a+="$k=${BASH_ALIASES[$k]}"$'\x1e'; done
    a=${a%$'\x1e'}
    a=${a//[$'\a\e']/}
    if [[ $a != "$__lig_aliases" ]]; then
      __lig_aliases=$a
      printf '\e]6973;A;%s\a' "$a" >/dev/tty
    fi
    local e=
    for k in $(compgen -e); do [[ $k == _ ]] || e+="$k=${!k}"$'\x1e'; done
    e=${e%$'\x1e'}
    e=${e//[$'\a\e']/}
    if [[ $e != "$__lig_vars" ]]; then
      __lig_vars=$e
      printf '\e]6973;V;%s\a' "$e" >/dev/tty
    fi
    if [[ -z $__lig_once ]]; then
      __lig_once=1
      local -a b f
      local x
      while IFS= read -r x; do b+=("$x"); done < <(compgen -b)
      while IFS= read -r x; do f+=("$x"); done < <(compgen -A function -X '_*')
      local IFS=$'\x1e'
      printf '\e]6973;K;%s\a\e]6973;N;%s\a' "${b[*]}" "${f[*]}" >/dev/tty
    fi
    # Re-append the marker when another tool (starship, …) rebuilds PS1.
    if [[ $PS1 != *'6973;S'* ]]; then
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
  # Prepended: vte.sh (GNOME Terminal) ends PS0 with `ESC \`, which would swallow our `\e`.
  [[ $PS0 == *'6973;F'* ]] || PS0='\e]6973;F\a'"${PS0:-}"

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
    if (( del >= ${#left} )); then
      left=
    elif (( del > 0 )); then
      left=${left:0:${#left}-del}
    fi
    READLINE_LINE=$left$text$right
    READLINE_POINT=$(( ${#left} + ${#text} - back ))
  }
  bind -x '"\e[9147~": __lig_insert'

  # Ghostty's prompt marks (OSC 133) tell it the shell is idle, so closing it needs no confirmation.
  if [[ -n $LIG_GHOSTTY ]]; then
    unset LIG_GHOSTTY
    if ! declare -F __ghostty_precmd >/dev/null && [[ -r $GHOSTTY_RESOURCES_DIR/shell-integration/bash/ghostty.bash ]]; then
      builtin source "$GHOSTTY_RESOURCES_DIR/shell-integration/bash/ghostty.bash"
    fi
  fi
fi

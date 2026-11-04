# lig ssh: back to the user's own startup files, then load lig after .zshrc.
if (( ${+LIG_ZDOTDIR} )); then ZDOTDIR=$LIG_ZDOTDIR; else unset ZDOTDIR; fi
typeset -g _lig_dir=$LIG_DIR
unset LIG_ZDOTDIR LIG_DIR
[[ -r ${ZDOTDIR:-$HOME}/.zshenv ]] && builtin source ${ZDOTDIR:-$HOME}/.zshenv
_lig_boot() {
  precmd_functions=(${precmd_functions:#_lig_boot})
  builtin source $_lig_dir/post.zsh
  [[ -n $_lig_dir ]] && command rm -rf -- $_lig_dir
  unset _lig_dir
  (( ${+functions[_lig_precmd]} )) && _lig_precmd
}
typeset -ga precmd_functions
precmd_functions+=(_lig_boot)

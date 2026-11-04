# lig ssh: the startup files of a login bash, then lig's hooks.
_lig_dir=$LIG_DIR
unset LIG_DIR
_lig_cleanup() {
  [[ -n $_lig_dir ]] && command rm -rf -- "$_lig_dir"
  unset _lig_dir
  unset -f _lig_cleanup
}
[[ -r /etc/profile ]] && . /etc/profile
for _lig_f in ~/.bash_profile ~/.bash_login ~/.profile ''; do
  [[ -z $_lig_f ]] && { [[ -r ~/.bashrc ]] && . ~/.bashrc; break; }
  [[ -r $_lig_f ]] && { . "$_lig_f"; break; }
done
unset _lig_f

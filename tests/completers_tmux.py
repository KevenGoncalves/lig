#!/usr/bin/env python3
"""The shell's own completions fill in for commands Lig has no spec for (issue #38:
behavior.completers = ["shell"]), in zsh and fish, in a private tmux server.
Run after cargo build --release: python3 tests/completers_tmux.py."""
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from fish_pty import BIN

# argc and a folder of argc scripts, as argc-completions sets up (LIG_TEST_ARGC=path/to/argc).
ARGC = os.environ.get('LIG_TEST_ARGC') or shutil.which('argc')
ARGC_SCRIPT = """# @cmd Deploy the app
# @arg env[staging|production]
deploy() { :; }
# @cmd Show the logs
logs() { :; }
eval "$(argc --argc-eval "$0" "$@")"
"""
SHELLS = {'zsh': os.environ.get('LIG_TEST_ZSH') or shutil.which('zsh'), 'fish': shutil.which('fish'),
          'bash': shutil.which('bash')}
BASH_COMPLETION = '/usr/share/bash-completion/bash_completion'
RC = {
    'bash': ('.bashrc', "PS1='comp> '\n"
             f"[ -r {BASH_COMPLETION} ] && . {BASH_COMPLETION}\n"
             "ligwords() { :; }\ncomplete -W 'alpha beta gamma' ligwords\n"),
    'zsh': ('.zshrc', "PROMPT='comp> '\nautoload -Uz compinit && compinit -u -d $HOME/.zcompdump\n"),
    'fish': ('.config/fish/config.fish', "function fish_prompt; echo -n 'comp> '; end\nset -g fish_greeting\n"),
}


@unittest.skipUnless(shutil.which('tmux'), 'tmux is required')
class CompletersTmux(unittest.TestCase):
    def start(self, kind, completers='["shell"]', extra_env=None, rc_extra=''):
        shell = SHELLS[kind]
        if not shell:
            self.skipTest(f'{kind} is unavailable')
        self.dir = tempfile.TemporaryDirectory(prefix='lig-comp-', ignore_cleanup_errors=True)
        directory = Path(self.dir.name)
        home, runtime = directory / 'home', directory / 'runtime'
        (home / '.config/lig').mkdir(parents=True)
        runtime.mkdir(mode=0o700)
        name, text = RC[kind]
        (home / name).parent.mkdir(parents=True, exist_ok=True)
        (home / name).write_text(text + rc_extra)
        # Ubuntu's /etc/zsh/zshrc runs compinit, which can stop to ask about folders' permissions.
        (home / '.zshenv').write_text('skip_global_compinit=1\n')
        (home / '.config/lig/config.toml').write_text(f'[behavior]\ncompleters = {completers}\nhelp_parsing = false\n')
        self.state = directory / 'state'
        env = {'HOME': str(home), 'ZDOTDIR': str(home), 'HISTFILE': str(home / '.history'), 'XDG_CONFIG_HOME': str(home / '.config'),
               'XDG_DATA_HOME': str(home / '.local/share'), 'XDG_CACHE_HOME': str(home / '.cache'),
               'XDG_STATE_HOME': str(self.state), 'XDG_RUNTIME_DIR': str(runtime), 'SHELL': shell, 'LIG_LOG': '1',
               **(extra_env or {})}
        subprocess.run([BIN, 'install', kind], env=dict(os.environ, **env), check=True, capture_output=True)
        self.base = ['tmux', '-S', str(directory / 'socket'), '-f', '/dev/null']
        self.addCleanup(lambda: (subprocess.run(self.base + ['kill-server'], capture_output=True), self.dir.cleanup()))
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'INSIDE_EMACS'] + [f'{k}={v}' for k, v in env.items()]
        self.tmux('new-session', '-d', '-x', '100', '-y', '30', '-s', 'test', '-c', str(home), *launch, shell)
        self.wait('no prompt', lambda s: 'comp>' in s, timeout=15)

    def tmux(self, *args):
        return subprocess.run(self.base + list(args), capture_output=True, text=True, timeout=5, check=True).stdout

    def screen(self):
        return self.tmux('capture-pane', '-p', '-t', 'test:0.0')

    def wait(self, what, check, timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if check(self.screen()):
                return
            time.sleep(.1)
        log = self.state / 'lig/lig.log'
        self.fail(f'{what}; pane:\n{self.screen()}\nlig.log:\n{log.read_text()[-2000:] if log.exists() else ""}')

    def popup(self, s):
        return ''.join(l for l in s.splitlines() if '│' in l)

    def prompt(self, s):
        return [l for l in s.splitlines() if l.startswith('comp>')][-1].rstrip()

    def test_zsh_offers_what_its_tab_would(self):
        self.start('zsh')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'setopt auto_c')
        self.wait("no setopt options from zsh", lambda s: 'auto_cd' in self.popup(s) and 'auto_continue' in self.popup(s))
        self.assertEqual(self.prompt(self.screen()), 'comp> setopt auto_c', 'asking zsh changed the line')
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('auto_cd not inserted', lambda s: self.prompt(s) == 'comp> setopt auto_cd')

    def test_zsh_hooks_stay_quiet_in_a_child_shell(self):
        """Issue #67: zsh-autosuggestions' completion strategy edits the line in a zpty child,
        which inherits lig's hooks; its line isn't the one being typed."""
        rc = (
            "foo() { :; }; _foo() { compadd alpha beta }; compdef _foo foo\n"
            "zmodload zsh/zpty\n"
            # What the plugin does after a widget: complete the line in a child, for the suggestion.
            "_suggest() { zpty -b lig_child 'BUFFER+=al; zle complete-word'; sleep .1; zpty -d lig_child; }\n"
            "_suggest_insert() { zle .self-insert; _suggest }\n"
            "zle -N self-insert _suggest_insert\n"
            # It wraps every widget at the first prompt, lig's own one that asks zsh too.
            "_suggest_wrap() {\n"
            "  precmd_functions=(${precmd_functions:#_suggest_wrap})\n"
            "  zle -A _lig_ask _suggest_orig_ask\n"
            "  _suggest_ask() { zle _suggest_orig_ask; _suggest }\n"
            "  zle -N _lig_ask _suggest_ask\n"
            "}\n"
            "precmd_functions+=(_suggest_wrap)\n"
        )
        self.start('zsh', rc_extra=rc)
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'foo ')
        self.wait('no alpha and beta from zsh', lambda s: 'alpha' in self.popup(s) and 'beta' in self.popup(s))
        self.assertNotIn('Asking zsh', self.popup(self.screen()))

    def test_zsh_is_asked_again_when_its_answer_is_lost_or_for_another_line(self):
        """A lost answer, or one for another line, left the popup on "Asking zsh" for good."""
        answers = {
            'lost': "",
            'another line': "builtin print -rn -- $'\\e]6973;Z;bar \\x1d\\a' >>$_lig_out",
        }
        for name, first in answers.items():
            with self.subTest(name):
                rc = (
                    "foo() { :; }; _foo() { compadd alpha beta }; compdef _foo foo\n"
                    # The first time lig asks, zsh answers `first` instead.
                    "_flaky_wrap() {\n"
                    "  precmd_functions=(${precmd_functions:#_flaky_wrap})\n"
                    "  zle -A _lig_ask _flaky_orig_ask\n"
                    f"  _flaky_ask() {{ if (( ! ${{+_flaky_done}} )); then _flaky_done=1; {first or ':'}; "
                    "else zle _flaky_orig_ask; fi; }\n"
                    "  zle -N _lig_ask _flaky_ask\n"
                    "}\n"
                    "precmd_functions+=(_flaky_wrap)\n"
                )
                self.start('zsh', rc_extra=rc)
                self.tmux('send-keys', '-t', 'test:0.0', '-l', 'foo ')
                self.wait('zsh not asked again', lambda s: 'alpha' in self.popup(s) and 'beta' in self.popup(s))
                self.doCleanups()

    def test_fish_offers_what_its_tab_would(self):
        self.start('fish')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'string esc')
        self.wait("no string subcommands from fish", lambda s: 'escape' in self.popup(s))
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('escape not inserted', lambda s: self.prompt(s) == 'comp> string escape')

    def test_bash_offers_what_its_tab_would(self):
        self.start('bash')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'ligwords b')
        self.wait("no words from bash's complete -W", lambda s: 'beta' in self.popup(s) and 'alpha' not in self.popup(s))
        self.assertEqual(self.prompt(self.screen()), 'comp> ligwords b', 'asking bash changed the line')
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('beta not inserted', lambda s: self.prompt(s) == 'comp> ligwords beta')

    @unittest.skipUnless(os.path.exists(BASH_COMPLETION) and shutil.which('timedatectl'), 'bash-completion and timedatectl')
    def test_bash_loads_a_completion_it_hasnt_yet(self):
        # bash-completion loads timedatectl's completion on first use (`complete -D`).
        self.start('bash')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'timedatectl set-')
        self.wait('no timedatectl subcommands', lambda s: 'set-timezone' in self.popup(s) and 'set-ntp' in self.popup(s))

    @unittest.skipUnless(ARGC, 'argc is unavailable')
    def test_argc_completes_the_commands_it_has_a_script_for(self):
        scripts = Path(tempfile.mkdtemp(prefix='lig-argc-'))
        self.addCleanup(shutil.rmtree, scripts)
        (scripts / 'ligdeploy.sh').write_text(ARGC_SCRIPT)
        path = f"{Path(ARGC).resolve().parent}:{os.environ['PATH']}"
        self.start('zsh', completers='["argc"]', extra_env={'ARGC_COMPLETIONS_PATH': str(scripts), 'PATH': path})
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'ligdeploy ')
        self.wait('no subcommands from argc', lambda s: 'Deploy the app' in self.popup(s) and 'logs' in self.popup(s))
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'deploy p')
        self.wait('no values from argc', lambda s: 'production' in self.popup(s) and 'staging' not in self.popup(s))
        self.tmux('send-keys', '-t', 'test:0.0', 'Enter')
        self.wait('production not inserted', lambda s: self.prompt(s) == 'comp> ligdeploy deploy production')

    def test_off_by_default(self):
        self.start('zsh', completers='[]')
        self.tmux('send-keys', '-t', 'test:0.0', '-l', 'setopt auto_c')
        time.sleep(2)
        self.assertNotIn('auto_cd', self.popup(self.screen()))


if __name__ == '__main__':
    unittest.main(verbosity=2)

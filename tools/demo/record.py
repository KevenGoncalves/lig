#!/usr/bin/env python3
"""Records assets/demo.gif: lig in zsh in a private tmux server, with a scratch HOME.

    cargo build --release && python3 tools/demo/record.py [out.gif]

Needs tmux, zsh, git, make, ssh, Docker and Pillow, plus a Nerd Font (JetBrainsMono Nerd Font
Mono by default; LIG_DEMO_FONT=path picks another). `lig ssh` logs in to a container built from
tools/demo/sshd.Dockerfile. The scenes are in scenes() at the bottom."""
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / 'target/release/lig'
COLS, ROWS = 92, 19
FONT_SIZE, CELL_W, CELL_H = 16, 10, 22
PAD, BAR = 18, 34
BG, FG = (18, 18, 18), (208, 208, 208)
# The 16 ANSI colors of a dark theme; the rest of the 256 are computed.
ANSI = [(40, 40, 40), (224, 108, 117), (152, 195, 121), (229, 192, 123), (97, 175, 239), (198, 120, 221),
        (86, 182, 194), (171, 178, 191), (92, 99, 112), (240, 128, 136), (170, 215, 140), (240, 205, 140),
        (120, 190, 250), (215, 140, 235), (110, 200, 210), (230, 230, 230)]


def color256(n):
    if n < 16:
        return ANSI[n]
    if n < 232:
        n -= 16
        level = lambda v: 0 if v == 0 else 55 + 40 * v
        return level(n // 36), level(n // 6 % 6), level(n % 6)
    v = 8 + 10 * (n - 232)
    return v, v, v


def font(bold=False):
    path = os.environ.get('LIG_DEMO_FONT')
    if not path:
        style = 'Bold' if bold else 'Regular'
        path = subprocess.run(['fc-match', '-f', '%{file}', f'JetBrainsMono Nerd Font Mono:style={style}'],
                              capture_output=True, text=True).stdout
    return ImageFont.truetype(path, FONT_SIZE)


SGR = re.compile(r'\x1b\[([0-9;:]*)m')


def parse(text):
    """Rows of (char, fg, bg, bold) from `tmux capture-pane -e`."""
    rows = []
    fg, bg, bold, reverse = None, None, False, False
    for line in text.split('\n')[:ROWS]:
        row, pos = [], 0
        for m in list(SGR.finditer(line)) + [None]:
            end = m.start() if m else len(line)
            for ch in line[pos:end]:
                f, b = (bg or BG, fg or FG) if reverse else (fg or FG, bg)
                row.append((ch, f, b, bold))
            if not m:
                break
            pos = m.end()
            codes = [int(c) if c else 0 for c in re.split('[;:]', m.group(1))]
            i = 0
            while i < len(codes):
                c = codes[i]
                if c == 0:
                    fg, bg, bold, reverse = None, None, False, False
                elif c == 1:
                    bold = True
                elif c == 22:
                    bold = False
                elif c == 7:
                    reverse = True
                elif c == 27:
                    reverse = False
                elif c in (38, 48) and i + 1 < len(codes):
                    if codes[i + 1] == 5 and i + 2 < len(codes):
                        value, i = color256(codes[i + 2]), i + 2
                    elif codes[i + 1] == 2 and i + 4 < len(codes):
                        value, i = tuple(codes[i + 2:i + 5]), i + 4
                    else:
                        value = None
                    if c == 38:
                        fg = value
                    else:
                        bg = value
                elif c == 39:
                    fg = None
                elif c == 49:
                    bg = None
                elif 30 <= c <= 37:
                    fg = ANSI[c - 30]
                elif 40 <= c <= 47:
                    bg = ANSI[c - 40]
                elif 90 <= c <= 97:
                    fg = ANSI[c - 82]
                elif 100 <= c <= 107:
                    bg = ANSI[c - 92]
                i += 1
        rows.append(row)
    return rows


class Renderer:
    def __init__(self):
        self.regular, self.bold = font(), font(bold=True)
        self.size = (PAD * 2 + COLS * CELL_W, BAR + PAD * 2 + ROWS * CELL_H)

    def frame(self, text, cursor):
        # Square corners: GIF has no partial transparency for rounded ones on a light page.
        img = Image.new('RGB', self.size, BG)
        d = ImageDraw.Draw(img)
        d.rectangle((0, 0, self.size[0] - 1, BAR - 1), fill=(38, 38, 38))
        for i, c in enumerate([(255, 95, 87), (254, 188, 46), (40, 200, 64)]):
            x = 22 + 22 * i
            d.ellipse((x - 6, BAR // 2 - 6, x + 6, BAR // 2 + 6), fill=c)
        title = 'zsh — lig'
        d.text((self.size[0] / 2, BAR / 2), title, font=self.regular, fill=(150, 150, 150), anchor='mm')
        top = BAR + PAD
        for y, row in enumerate(parse(text)):
            for x, (ch, fg, bg, bold) in enumerate(row[:COLS]):
                left, cy = PAD + x * CELL_W, top + y * CELL_H
                if bg:
                    d.rectangle((left, cy, left + CELL_W - 1, cy + CELL_H - 1), fill=bg)
                if ch != ' ':
                    d.text((left, cy + CELL_H / 2), ch, font=self.bold if bold else self.regular, fill=fg, anchor='lm')
        if cursor:
            x, y = cursor
            left, cy = PAD + x * CELL_W, top + y * CELL_H
            d.rectangle((left, cy + 2, left + 1, cy + CELL_H - 3), fill=FG)
        return img


class Server:
    """A container with sshd on a local port, that takes the key in `key`."""
    def __init__(self, key, port=2222):
        self.port = port
        subprocess.run(['docker', 'build', '-q', '-t', 'lig-demo-sshd', '-f', ROOT / 'tools/demo/sshd.Dockerfile',
                        ROOT / 'tools/demo'], check=True, capture_output=True)
        self.id = subprocess.run(['docker', 'run', '-d', '--rm', '--hostname', 'server', '-p',
                                  f'127.0.0.1:{port}:22', 'lig-demo-sshd'],
                                 check=True, capture_output=True, text=True).stdout.strip()
        subprocess.run(['docker', 'exec', '-i', '-u', 'demo', self.id, 'sh', '-c',
                        'cat > ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys'],
                       input=Path(f'{key}.pub').read_bytes(), check=True)

    def stop(self):
        subprocess.run(['docker', 'kill', self.id], capture_output=True)


class Session:
    def __init__(self, directory):
        self.dir = directory
        self.home = directory / 'home'
        self.project = self.home / 'project'
        self.setup()
        self.frames = []
        self.stop = threading.Event()
        self.server = Server(self.home / '.ssh/id_ed25519')

    def setup(self):
        home, runtime = self.home, self.dir / 'runtime'
        shutil.copytree(ROOT / 'tools/demo/project', self.project)
        runtime.mkdir(mode=0o700)
        self.env = {'HOME': str(home), 'ZDOTDIR': str(home), 'XDG_CONFIG_HOME': str(home / '.config'),
                    'XDG_DATA_HOME': str(home / '.local/share'), 'XDG_CACHE_HOME': str(home / '.cache'),
                    'XDG_STATE_HOME': str(home / '.local/state'), 'XDG_RUNTIME_DIR': str(runtime),
                    'SHELL': shutil.which('zsh'), 'TERM': 'xterm-256color', 'LANG': 'C.UTF-8',
                    'HISTFILE': str(home / '.zsh_history'), 'PATH': f'{BIN.parent}:/usr/local/bin:/usr/bin:/bin',
                    'GIT_CONFIG_GLOBAL': str(home / '.gitconfig')}
        git = lambda *a: subprocess.run(['git', *a], cwd=self.project, env=dict(os.environ, **self.env),
                                        check=True, capture_output=True)
        (home / '.gitconfig').write_text('[user]\n\tname = demo\n\temail = demo@example.com\n'
                                         '[init]\n\tdefaultBranch = main\n')
        git('init', '-q')
        git('add', '.')
        git('commit', '-qm', 'first commit')
        for branch, message in [('feature/login', 'login form'), ('fix/typo', 'fix a typo'), ('docs/usage', 'usage')]:
            git('switch', '-qc', branch, 'main')
            git('commit', '-q', '--allow-empty', '-m', message)
        git('switch', '-q', 'main')
        (home / '.zshenv').write_text('skip_global_compinit=1\n')
        # ssh reads ~/.ssh from the passwd entry, not $HOME: -F points it at the demo's.
        (home / '.ssh').mkdir(mode=0o700)
        (home / '.ssh/config').write_text(
            f'Host server\n  HostName 127.0.0.1\n  Port 2222\n  User demo\n  IdentityFile {home}/.ssh/id_ed25519\n'
            '  IdentitiesOnly yes\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n  LogLevel ERROR\n')
        subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', home / '.ssh/id_ed25519'], check=True)
        (home / '.zshrc').write_text("PROMPT='%F{blue}%~%f %F{green}❯%f '\nalias gco='git checkout'\nexport EDITOR=nvim\n"
                                     f"alias ssh='lig ssh -F {home}/.ssh/config'\n"
                                     'setopt INC_APPEND_HISTORY\n')
        now = int(time.time())
        history = ['make release', 'docker compose up -d', 'git log --oneline --graph',
                   'cargo build --release', 'npm run dev', 'ssh deploy@staging', 'git push origin main',
                   'kubectl get pods -n production', 'python3 src/greet.py --name world']
        (home / '.zsh_history').write_text(''.join(f': {now - 600 + i}:0;{c}\n' for i, c in enumerate(history)))
        (home / '.config/lig').mkdir(parents=True)
        (home / '.config/lig/config.toml').write_text('[ui]\nicons = "nerd"\nborder = "rounded"\n'
                                                      '[behavior]\nonline = true\n')
        subprocess.run([BIN, 'install', 'zsh'], env=dict(os.environ, **self.env), check=True, capture_output=True)
        self.base = ['tmux', '-L', f'lig-demo-{os.getpid()}', '-f', '/dev/null']

    def tmux(self, *args):
        return subprocess.run(self.base + list(args), capture_output=True, text=True, timeout=5, check=True).stdout

    def start(self):
        launch = ['env', '-u', 'LIG_TTY', '-u', 'LIG_DISABLE', '-u', 'TMUX', '-u', 'SSH_AUTH_SOCK'] + [f'{k}={v}' for k, v in self.env.items()]
        self.tmux('new-session', '-d', '-x', str(COLS), '-y', str(ROWS), '-s', 'demo', '-c', str(self.project),
                  *launch, 'zsh')
        self.tmux('set', '-g', 'status', 'off')
        self.wait('❯')
        self.tmux('send-keys', '-t', 'demo', 'C-l')
        time.sleep(1)
        self.thread = threading.Thread(target=self.capture, daemon=True)
        self.start_time = time.monotonic()
        self.thread.start()

    def screen(self, colors=False):
        return self.tmux('capture-pane', '-p', '-N', *(['-e'] if colors else []), '-t', 'demo')

    def capture(self):
        last = None
        while not self.stop.is_set():
            try:
                text = self.screen(colors=True)
                x, y, shown = self.tmux('display', '-p', '-t', 'demo', '#{cursor_x} #{cursor_y} #{cursor_flag}').split()
            except subprocess.SubprocessError:
                continue
            state = (text, (int(x), int(y)) if shown == '1' else None)
            if state != last:
                self.frames.append((time.monotonic() - self.start_time, *state))
                last = state
            time.sleep(0.03)

    def wait(self, needle, timeout=10):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.screen():
                return
            time.sleep(0.05)
        raise SystemExit(f'waited for {needle!r}; pane:\n{self.screen()}')

    def type(self, text, delay=0.09):
        for ch in text:
            self.tmux('send-keys', '-t', 'demo', '-l', ch)
            time.sleep(delay)

    def key(self, *names, pause=0.45):
        for name in names:
            self.tmux('send-keys', '-t', 'demo', name)
            time.sleep(pause)

    def finish(self):
        time.sleep(0.3)
        self.stop.set()
        self.thread.join()
        self.tmux('kill-server')
        self.server.stop()


def scenes(s):
    # Subcommands with what they do, then the repo's branches.
    s.type('git c')
    time.sleep(1.6)
    s.key('Enter', pause=0.5)
    s.type(' ')
    s.wait('feature/login')
    time.sleep(0.8)
    s.key('Down', 'Down')
    s.key('Enter', pause=0.6)
    s.key('Enter', pause=1.4)
    # Options with descriptions, and their values.
    s.type('git commit --')
    time.sleep(1.5)
    s.type('am')
    time.sleep(1.2)
    s.key('C-u', pause=0.5)
    # The Makefile's targets.
    s.type('make ')
    time.sleep(1.5)
    s.key('C-u', pause=0.5)
    # Aliases complete like what they stand for; variables too.
    s.type('gco ')
    time.sleep(1.5)
    s.key('C-u', pause=0.5)
    s.type('echo $EDI')
    time.sleep(1.3)
    s.key('C-u', pause=0.5)
    # Packages searched online.
    s.type('npm install expr')
    s.wait('express', timeout=15)
    time.sleep(1.8)
    s.key('C-u', pause=0.5)
    # A server through `lig ssh`: its hosts, then the popup for the server's own branches.
    s.key('C-l', pause=0.5)
    s.type('ssh ser')
    time.sleep(1.2)
    s.key('Enter', pause=0.4)
    s.key('Enter')
    s.wait('demo@server', timeout=20)
    time.sleep(1.0)
    s.type('git checkout ')
    s.wait('hotfix/payments')
    time.sleep(1.6)
    s.key('Down', 'Down')
    s.key('Enter', pause=0.4)
    s.key('Enter', pause=1.2)
    s.type('cat logs/')
    time.sleep(1.4)
    s.key('C-u', pause=0.4)
    s.type('exit\r', delay=0.07)
    time.sleep(1.0)
    s.key('C-l', pause=0.5)
    # History: the commands that worked, fuzzy-searched.
    s.key('C-r', pause=1.2)
    s.type('make rel', delay=0.15)
    time.sleep(1.4)
    s.key('Enter', pause=0.8)
    s.key('Enter', pause=1.5)


def main():
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'assets/demo.gif')
    with tempfile.TemporaryDirectory(prefix='lig-demo-') as tmp:
        s = Session(Path(tmp))
        s.start()
        try:
            scenes(s)
        finally:
            s.finish()
    renderer = Renderer()
    frames, durations = [], []
    for i, (t, text, cursor) in enumerate(s.frames):
        end = s.frames[i + 1][0] if i + 1 < len(s.frames) else t + 2.5
        ms = max(20, round((end - t) * 100) * 10)
        frames.append(renderer.frame(text, cursor))
        durations.append(ms)
    frames[0].save(out, save_all=True, append_images=frames[1:], duration=durations, loop=0, optimize=True)
    print(f'{out}: {len(frames)} frames, {sum(durations) / 1000:.1f}s, {out.stat().st_size // 1024} KiB')


if __name__ == '__main__':
    main()

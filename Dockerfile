# Try lig without installing it, in zsh, bash or fish:
#
#   docker build -t lig-demo https://github.com/KevenGoncalves/lig.git
#   docker run --rm -it lig-demo          # zsh; or add bash or fish
#
# From a clone, build with `docker build -t lig-demo .` instead.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p lig && cp target/release/lig /lig

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash zsh fish git make less procps curl ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /lig /usr/local/bin/lig
RUN useradd --create-home --shell /usr/bin/zsh demo
USER demo
ENV TERM=xterm-256color LANG=C.UTF-8
WORKDIR /home/demo/project

# A small git project, so branches, files and make targets have something to show.
COPY --chown=demo:demo tools/demo/project/ /home/demo/project/
COPY --chown=demo:demo tools/demo/welcome /home/demo/.welcome
RUN git config --global user.name demo && git config --global user.email demo@example.com \
    && git config --global init.defaultBranch main \
    && git init -q && git add . && git commit -qm "first commit" \
    && git switch -qc feature/login && git commit -q --allow-empty -m "login form" \
    && git switch -qc fix/typo main && git commit -q --allow-empty -m "fix a typo" \
    && git switch -q main \
    && for shell in zsh bash fish; do lig install $shell >/dev/null; done \
    && printf '%s\n' "PROMPT='%F{blue}%~%f %# '" >> ~/.zshrc \
    && printf '[ -z "$LIG_WELCOMED" ] && export LIG_WELCOMED=1 && cat ~/.welcome\n' >> ~/.zshrc \
    && printf '[ -z "$LIG_WELCOMED" ] && export LIG_WELCOMED=1 && cat ~/.welcome\n' >> ~/.bashrc \
    && printf 'set -g fish_greeting\nstatus is-interactive; and not set -q LIG_WELCOMED; and set -gx LIG_WELCOMED 1; and cat ~/.welcome\n' \
        >> ~/.config/fish/config.fish

# lig would be PID 1 otherwise, and it leaves orphans unreaped.
ENTRYPOINT ["tini", "--"]
CMD ["zsh"]

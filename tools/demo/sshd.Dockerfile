# The server tools/demo/record.py logs in to with `lig ssh`.
FROM debian:12
RUN apt-get update && apt-get install -y --no-install-recommends openssh-server zsh git \
    && rm -rf /var/lib/apt/lists/* && mkdir /run/sshd
RUN useradd -m -s /usr/bin/zsh demo && passwd -d demo
USER demo
WORKDIR /home/demo/app
RUN git config --global user.name demo && git config --global user.email demo@example.com \
    && git config --global init.defaultBranch main \
    && git init -q && git commit -q --allow-empty -m "v1.0" \
    && git switch -qc release/2.0 && git commit -q --allow-empty -m "release 2.0" \
    && git switch -qc hotfix/payments main && git commit -q --allow-empty -m "fix rounding" \
    && git switch -q main \
    && mkdir -p ~/.ssh logs config && touch logs/app.log config/app.toml \
    && printf 'skip_global_compinit=1\n' > ~/.zshenv \
    && printf "PROMPT='%%F{magenta}%%n@%%m%%f %%F{blue}%%~%%f %%F{green}❯%%f '\ncd ~/app\n" > ~/.zshrc
USER root
CMD ["/usr/sbin/sshd", "-D", "-e"]

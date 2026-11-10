# Apple Silicon macOS support

Approved scope: native aarch64-apple-darwin builds, installation, updates, shell integration, and zellij resurrection with the new explicit command API. Existing Linux builds and mirroring remain supported. No Intel macOS release.

## Approaches

A direct argv-rewriting port was rejected: macOS retains argc, truncates longer commands, and can expose environment values when fewer arguments are mirrored. A fixed shell launcher preserves arguments but changes idle restoration. The approved approach uses native process inspection and zellij's `set-resurrect-command` API. An isolated test against upstream d06f87aca7151e032d0f10befb61f1a6df365a86 proved exact argv/cwd restoration, suspended execution until Enter, and plain idle-pane serialization.

## Design

Use a small platform process module for cwd, executable identity, descendants, tty ownership, foreground process group, and arguments. Keep Linux /proc implementation intact. On macOS use libc's libproc/sysctl bindings; avoid spawning process-list commands on every keystroke. Share resurrection exclusion rules with Linux. Invoke zellij with argument vectors, explicit session and pane IDs, bounded runtime, and only on changed state. Fail closed to a shell for excluded/oversized commands. Respect LIG_NO_MIRROR; unsupported zellij must leave autocomplete working and be reported by doctor. Capability detection, not an unreleased version number, gates support.

Release an unsigned native CLI archive with SHA-256 verification using tools available on macOS. Preserve Linux targets, add macOS tests and package generation, and document platform paths and the existing bash version requirement (Apple's bundled bash is too old). Nix should advertise aarch64-darwin only for macOS.

## Validation

Native Rust and shell PTY tests, platform parser tests, installer target/checksum tests, and real Lig/zellij save-resurrect tests on Apple Silicon. Linux tests remain in CI; distinguish locally executed tests from CI not run here. No release publishing or user shell installation.

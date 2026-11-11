# Written by the release workflow of KevenGoncalves/lig (packaging/homebrew); edits here are replaced.
class Lig < Formula
  desc "Fig-style IDE autocomplete for your terminal (zsh, bash and fish)"
  homepage "https://github.com/KevenGoncalves/lig"
  license "MIT"

  on_macos do
    depends_on arch: :arm64

    on_arm do
      url "https://github.com/KevenGoncalves/lig/releases/download/v@VERSION@/lig-@VERSION@-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA_aarch64-apple-darwin@"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/KevenGoncalves/lig/releases/download/v@VERSION@/lig-@VERSION@-x86_64-unknown-linux-musl.tar.gz"
      sha256 "@SHA_x86_64-unknown-linux-musl@"
    end
    on_arm do
      url "https://github.com/KevenGoncalves/lig/releases/download/v@VERSION@/lig-@VERSION@-aarch64-unknown-linux-musl.tar.gz"
      sha256 "@SHA_aarch64-unknown-linux-musl@"
    end
  end

  def install
    bin.install "lig"
  end

  def caveats
    <<~EOS
      Run `lig install` to turn on the popup in your shell (zsh, bash or fish).
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lig --version")
  end
end

class Ports < Formula
  desc "Inspect and manage listening ports and network connections"
  homepage "https://github.com/noahlin34/ports"
  version "0.1.0"
  license "MIT"

  # The release workflow renders the exact tag URLs and SHA-256 values, then
  # commits the updated formula here and to noahlin34/homebrew-tap.
  on_arm do
    url "https://github.com/noahlin34/ports/releases/download/v0.1.0/ports-macos-arm64.tar.gz"
    sha256 "a574e5448e272ce1d047c6778a4326459574cc328fc08a675cd893c54d338382"
  end

  on_intel do
    url "https://github.com/noahlin34/ports/releases/download/v0.1.0/ports-macos-x86_64.tar.gz"
    sha256 "6320eec952424b73431f6d8a7169f4f58111e51a2a142341dddca52f3e5523c1"
  end

  def install
    bin.install "ports"
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/ports --version"))
  end
end

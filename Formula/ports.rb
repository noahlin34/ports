class Ports < Formula
  desc "Inspect and manage listening ports and network connections"
  homepage "https://github.com/noahlin34/ports"
  license "MIT"

  # The release workflow renders the exact tag URLs and SHA-256 values, then
  # commits the updated formula here and to noahlin34/homebrew-tap.
  on_arm do
    url "https://github.com/noahlin34/ports/releases/latest/download/ports-macos-arm64.tar.gz"
  end

  on_intel do
    url "https://github.com/noahlin34/ports/releases/latest/download/ports-macos-x86_64.tar.gz"
  end

  def install
    bin.install "ports"
  end

  test do
    assert_match(/\d+\.\d+\.\d+/, shell_output("#{bin}/ports --version"))
  end
end
